/* ghostline_autosuggest.c — Bash loadable builtin for flicker-free auto-ghost.
 *
 * Hooks readline's own redisplay (rl_redisplay_function) so the ghost suffix
 * is drawn AS PART OF the normal redisplay, not as a second full-line
 * `bind -x` refresh. Typing stays native/incremental; only the ghost region
 * (after cursor) is painted per key. No `bind -x` printable hooks.
 *
 * Ship rules: fast-path cache (no fork when typing into the cached
 * suggestion); slow-path fork on every diverge (~1ms release, no extra
 * redraw, so no throttle — throttling leaves the old tail on screen while
 * native insertion overwrites its first char, rendering garbage like
 * `co`+`lear` → `coear`. Every redisplay must end with correct paint.
 *
 * Build: cc -shared -fPIC -o ghostline_autosuggest.so ghostline_autosuggest.c
 * Load:  enable -f ./ghostline_autosuggest.so ghostline_autosuggest
 *        ghostline_autosuggest enable
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <sys/stat.h>

#include <readline/readline.h>
#include <readline/keymaps.h>

#include "builtins.h"
#include "shell.h"
#include "command.h"
#include "builtins/common.h"

static rl_voidfunc_t *orig_redisplay = NULL;
static int g_enabled = 0;
static char *g_cached_full = NULL;   /* full suggestion: buffer+suffix */
static char *g_painted_suf = NULL;
static char *g_painted_line = NULL;
static char g_bin[PATH_MAX] = "";    /* absolute ghostline path, else "" */

/* orig key funcs (emacs map) for wrappers */
static rl_command_func_t *o_accept_m = NULL;
static rl_command_func_t *o_accept_j = NULL;
static rl_command_func_t *o_left = NULL;
static rl_command_func_t *o_home = NULL;
static rl_command_func_t *o_home1 = NULL;
static rl_command_func_t *o_beg = NULL;
static rl_command_func_t *o_back = NULL;
static rl_command_func_t *o_paste = NULL;
static rl_command_func_t *o_tab = NULL;
static rl_command_func_t *o_altf = NULL;
static int g_last_rows = -1, g_last_cols = -1;

static void free_str(char **p) { if (p && *p) { free(*p); *p = NULL; } }

static int has_control(const char *s) {
    for (; *s; s++) {
        unsigned char c = (unsigned char)*s;
        if (c < 0x20 || c == 0x7f) return 1;
    }
    return 0;
}

/* Validated SGR params only (digits + ';', <=32 chars). Default dim gray. */
static const char *ghost_style(void) {
    const char *e = getenv("GHOSTLINE_GHOST_STYLE");
    if (!e || !*e) return "2;38;5;244";
    size_t n = strlen(e);
    if (n == 0 || n > 32) return "2;38;5;244";
    for (size_t i = 0; i < n; i++) {
        if (!((e[i] >= '0' && e[i] <= '9') || e[i] == ';')) return "2;38;5;244";
    }
    return e;
}

static void resolve_bin(void) {
    g_bin[0] = '\0';
    const char *e = getenv("GHOSTLINE_BIN");
    if (e && *e && access(e, X_OK) == 0) {
        strncpy(g_bin, e, sizeof(g_bin) - 1);
        return;
    }
    /* No shell, no popen: walk PATH once at enable and cache absolute. */
    const char *path = getenv("PATH");
    if (!path || !*path) return;
    char tmp[PATH_MAX];
    const char *p = path;
    while (*p) {
        const char *c = strchr(p, ':');
        size_t dl = c ? (size_t)(c - p) : strlen(p);
        if (dl > 0 && dl + 12 < sizeof(tmp)) {
            memcpy(tmp, p, dl);
            tmp[dl] = '\0';
            size_t L = strlen(tmp);
            if (L > 0 && tmp[L-1] != '/') { tmp[L++] = '/'; tmp[L] = '\0'; }
            strcpy(tmp + L, "ghostline");
            if (access(tmp, X_OK) == 0) {
                strncpy(g_bin, tmp, sizeof(g_bin) - 1);
                return;
            }
        }
        if (!c) break;
        p = c + 1;
    }
}

static void reap_zombie(void) { (void)0; }

static void worker_kill(void) { (void)0; }

/* Sync fetch is ~1ms release and causes no redraw (ghost region only), so
 * fork on every diverge: every redisplay ends with correct paint or a clean
 * clear. A throttle here would leave the stale tail visible while the newly
 * typed char overwrites its first byte (renders e.g. `coear`). */
static char *fetch_suffix_sync(const char *buf, const char *cwd) {
    int fd[2];
    if (pipe(fd) != 0) return NULL;
    pid_t pid = fork();
    if (pid < 0) { close(fd[0]); close(fd[1]); return NULL; }
    if (pid == 0) {
        close(fd[0]);
        if (dup2(fd[1], STDOUT_FILENO) < 0) _exit(127);
        close(fd[1]);
        int dn = open("/dev/null", O_WRONLY);
        if (dn >= 0) { dup2(dn, STDERR_FILENO); close(dn); }
        if (*g_bin) execl(g_bin, "ghostline", "suggest", "--cwd", cwd ? cwd : ".", "--", buf, (char *)NULL);
        else execlp("ghostline", "ghostline", "suggest", "--cwd", cwd ? cwd : ".", "--", buf, (char *)NULL);
        _exit(127);
    }
    close(fd[1]);
    char tmp[4096]; size_t len = 0, cap = 256;
    char *out = malloc(cap);
    if (!out) { close(fd[0]); waitpid(pid, NULL, 0); return NULL; }
    ssize_t n;
    while ((n = read(fd[0], tmp, sizeof(tmp))) > 0) {
        if (len + (size_t)n + 1 > cap) {
            cap = (len + n + 1) * 2;
            if (cap > 8192) break;
            char *no = realloc(out, cap);
            if (!no) break;
            out = no;
        }
        memcpy(out + len, tmp, (size_t)n);
        len += (size_t)n;
        if (len > 4096) break;
    }
    close(fd[0]);
    waitpid(pid, NULL, 0);
    out[len] = '\0';
    while (len > 0 && (out[len-1] == '\n' || out[len-1] == '\r')) out[--len] = '\0';
    if (has_control(out)) { free(out); return strdup(""); }
    return out;
}

static char *current_cwd(void) {
    static char buf[PATH_MAX];
    if (getcwd(buf, sizeof(buf))) return buf;
    return ".";
}

static void out_write(const char *s) {
    FILE *out = rl_outstream ? rl_outstream : stdout;
    int fd = fileno(out);
    if (fd < 0) return;
    size_t n = strlen(s);
    while (n > 0) {
        ssize_t w = write(fd, s, n);
        if (w <= 0) break;
        s += w; n -= (size_t)w;
    }
}

static void paint_suffix(const char *suf) {
    out_write("\033[s\033[K\033[");
    out_write(ghost_style());
    out_write("m");
    out_write(suf);
    out_write("\033[m\033[u");
}

static void clear_tail(void) {
    out_write("\033[s\033[K\033[u");
}

static void drop_painted(void) {
    free_str(&g_painted_suf);
    free_str(&g_painted_line);
}

static void ghost_redisplay(void) {
    /* SIGWINCH fix: readline's rl_resize_terminal() skips
     * _rl_redisplay_after_sigwinch()'s clear when a custom
     * rl_redisplay_function is installed (terminal.c: custom ->
     * rl_forced_update_display() with no clear, vs default ->
     * _rl_redisplay_after_sigwinch() which clears). Without the clear,
     * orig draws at the stale cursor position, appending a second
     * prompt instead of overwriting (observed as
     * `[u@h ~]$ [u@h ~]$ ...` on every resize). Replicate the missing
     * clear when the screen size changed. Must run even when ghost is
     * disabled: the hook itself breaks the resize path, not the paint.
     * NOTE: single-line clear only (covers empty prompt + typical
     * single-line buffers, i.e. the reported bug). Multi-line prompts or
     * wrapped buffers occupying >1 screen line can still leave fragments
     * above, same as baseline readline custom-hook behavior; full
     * multi-line would need saved _rl_vis_botlin/_rl_last_v_pos. */
    {
        int rows = 0, cols = 0;
        int resized = 0;
        rl_get_screen_size(&rows, &cols);
        if (g_last_rows != -1 && (rows != g_last_rows || cols != g_last_cols))
            resized = 1;
        g_last_rows = rows;
        g_last_cols = cols;
        if (resized) {
            rl_clear_visible_line();
            /* Clear wiped the ghost too, but g_painted_* still thinks it is
             * on screen -> fast path would skip repaint and leave the line
             * ghostless until the next key. Drop to force repaint below. */
            drop_painted();
        }
    }
    if (orig_redisplay) orig_redisplay();
    else rl_redisplay();

    if (!g_enabled) return;
    const char *gg = getenv("GHOSTLINE_GHOST");
    if (gg && strcmp(gg, "0") == 0) return;
    if (getenv("GHOSTLINE_DISABLED")) return;
    const char *term = getenv("TERM");
    if (!term || !*term || strcmp(term, "dumb") == 0) return;
    const char *comp = getenv("COMP_LINE");
    if (comp && *comp) return;
    if (!rl_line_buffer) return;
    reap_zombie();
    if (rl_point != rl_end) {
        /* Movement wrappers already cleared the tail while at EOL.
         * Never emit K from mid cursor (would erase real line content). */
        free_str(&g_cached_full);
        drop_painted();
        worker_kill();
        return;
    }
    const char *buf = rl_line_buffer;
    size_t blen = strlen(buf);
    if (blen == 0 || blen > 200) {
        if (g_painted_suf) clear_tail();
        drop_painted();
        free_str(&g_cached_full);
        worker_kill();
        return;
    }
    /* fast path: typing forward into cache needs no worker */
    if (g_cached_full && strlen(g_cached_full) > blen
        && strncmp(g_cached_full, buf, blen) == 0 && buf[blen-1] != ' ') {
        const char *suf = g_cached_full + blen;
        if (!*suf || has_control(suf)) {
            if (g_painted_suf) clear_tail();
            drop_painted();
            return;
        }
        if (g_painted_suf && g_painted_line
            && strcmp(suf, g_painted_suf) == 0 && strcmp(buf, g_painted_line) == 0)
            return;
        free_str(&g_painted_suf); free_str(&g_painted_line);
        g_painted_suf = strdup(suf);
        g_painted_line = strdup(buf);
        paint_suffix(suf);
        return;
    }
    /* slow path: fork on every diverge. Fast path above already skips forks
     * while typing into the cache, so burst cost is bounded by typing rate. */
    char *suf = fetch_suffix_sync(buf, current_cwd());
    if (!suf || !*suf) {
        free(suf);
        free_str(&g_cached_full);
        if (g_painted_suf) clear_tail();
        drop_painted();
        return;
    }
    free_str(&g_cached_full);
    g_cached_full = malloc(blen + strlen(suf) + 1);
    if (g_cached_full) {
        memcpy(g_cached_full, buf, blen);
        strcpy(g_cached_full + blen, suf);
    }
    if (g_painted_suf && g_painted_line
        && strcmp(suf, g_painted_suf) == 0 && strcmp(buf, g_painted_line) == 0) {
        free(suf);
        return;
    }
    free_str(&g_painted_suf); free_str(&g_painted_line);
    g_painted_suf = suf;
    g_painted_line = strdup(buf);
    paint_suffix(suf);
}

/* Right / Ctrl-F: accept at EOL when usable, else forward-char. */
static int ghost_accept(int count, int key) {
    (void)count; (void)key;
    if (rl_line_buffer && rl_point == rl_end && g_cached_full) {
        size_t blen = strlen(rl_line_buffer);
        if (blen > 0 && strncmp(g_cached_full, rl_line_buffer, blen) == 0
            && strlen(g_cached_full) > blen) {
            const char *suf = g_cached_full + blen;
            if (*suf && !has_control(suf)) {
                rl_insert_text(suf);
                return 0;
            }
        }
    }
    return rl_forward_char(1, key);
}

static int ghost_accept_word(int count, int key) {
    (void)count; (void)key;
    if (rl_line_buffer && rl_point == rl_end && g_cached_full) {
        size_t blen = strlen(rl_line_buffer);
        if (blen > 0 && strncmp(g_cached_full, rl_line_buffer, blen) == 0) {
            const char *rem = g_cached_full + blen;
            if (*rem) {
                const char *p = rem;
                while (*p == ' ' || *p == '\t') p++;
                while (*p && *p != ' ' && *p != '\t') p++;
                if (p > rem) {
                    char *w = strndup(rem, (size_t)(p - rem));
                    if (w) { rl_insert_text(w); free(w); return 0; }
                }
            }
        }
    }
    if (o_altf) return o_altf(1, key);
    rl_command_func_t *f = rl_named_function("forward-word");
    if (f) return f(1, key);
    return rl_forward_char(1, key);
}

/* Tab: accept ghost when usable, else original completion.
 * Without this, typing `ope` with ghost `ncode` + Tab runs readline's
 * `complete`, which completes to the longest common prefix (`open`) across
 * PATH matches and discards the ghost. With the wrapper, first Tab takes
 * the ghost; Tab with no ghost preserves the user's completion binding
 * (complete / menu-complete / custom). */
static int ghost_tab(int count, int key) {
    if (rl_line_buffer && rl_point == rl_end && g_cached_full) {
        size_t blen = strlen(rl_line_buffer);
        if (blen > 0 && strncmp(g_cached_full, rl_line_buffer, blen) == 0
            && strlen(g_cached_full) > blen) {
            const char *suf = g_cached_full + blen;
            if (*suf && !has_control(suf)) {
                rl_insert_text(suf);
                return 0;
            }
        }
    }
    if (o_tab) return o_tab(count, key);
    rl_command_func_t *f = rl_named_function("complete");
    if (f) return f(count, key);
    return 0;
}

static int ghost_clear(int count, int key) {
    (void)count; (void)key;
    if (g_painted_suf) clear_tail();
    free_str(&g_cached_full);
    drop_painted();
    worker_kill();
    return 0;
}

static int ghost_end(int count, int key) {
    (void)count; (void)key;
    if (rl_line_buffer && rl_point == rl_end && g_cached_full) {
        size_t blen = strlen(rl_line_buffer);
        if (blen > 0 && strncmp(g_cached_full, rl_line_buffer, blen) == 0
            && strlen(g_cached_full) > blen) {
            const char *suf = g_cached_full + blen;
            if (*suf && !has_control(suf)) { rl_insert_text(suf); return 0; }
        }
    }
    return rl_end_of_line(1, key);
}

/* Enter: erase ghost tail (still at EOL) before the line executes, else the
 * ghost looks executed. Then run the original accept-line. */
static int ghost_newline(int count, int key) {
    if (g_painted_suf) clear_tail();
    free_str(&g_cached_full);
    drop_painted();
    worker_kill();
    rl_command_func_t *o = (key == '\r') ? o_accept_m : o_accept_j;
    if (o) return o(count, key);
    return rl_newline(count, key);
}

/* Movement/paste wrappers: clear while still at the old cursor (EOL clears
 * just the ghost; from mid it would erase real content, so redisplay path
 * handles that by dropping state only). */
static int ghost_move_wrap(int count, int key, rl_command_func_t *o,
                           int fallback_begin) {
    if (g_painted_suf && rl_line_buffer && rl_point == rl_end) clear_tail();
    free_str(&g_cached_full);
    drop_painted();
    if (o) return o(count, key);
    if (fallback_begin) return rl_beg_of_line(count, key);
    return rl_backward_char(count, key);
}
static int ghost_left(int c, int k) { return ghost_move_wrap(c, k, o_left, 0); }
static int ghost_home(int c, int k) { return ghost_move_wrap(c, k, o_home, 1); }
static int ghost_home1(int c, int k) { return ghost_move_wrap(c, k, o_home1, 1); }
static int ghost_beg(int c, int k) { return ghost_move_wrap(c, k, o_beg, 1); }
static int ghost_back(int c, int k) { return ghost_move_wrap(c, k, o_back, 0); }
static int ghost_paste(int c, int k) {
    free_str(&g_cached_full);
    drop_painted();
    worker_kill();
    if (o_paste) return o_paste(c, k);
    return 0;
}

static rl_command_func_t *saved_func(const char *seq) {
    Keymap em = rl_get_keymap_by_name("emacs");
    if (!em) em = rl_get_keymap();
    int type = 0;
    rl_command_func_t *f = rl_function_of_keyseq(seq, em, &type);
    return (type == ISFUNC) ? f : NULL;
}

static void bind_both(const char *seq, rl_command_func_t *fn) {
    rl_bind_keyseq(seq, fn);
    Keymap vim = rl_get_keymap_by_name("vi-insert");
    if (vim) rl_bind_keyseq_in_map(seq, fn, vim);
}

static void bind_wrap_both(const char *seq, rl_command_func_t *fn,
                           rl_command_func_t **saved) {
    if (!*saved) *saved = saved_func(seq);
    bind_both(seq, fn);
}

static int do_enable(void) {
    if (!orig_redisplay) orig_redisplay = rl_redisplay_function;
    rl_redisplay_function = ghost_redisplay;
    rl_add_funmap_entry("ghostline-accept", ghost_accept);
    rl_add_funmap_entry("ghostline-accept-word", ghost_accept_word);
    rl_add_funmap_entry("ghostline-accept-tab", ghost_tab);
    rl_add_funmap_entry("ghostline-clear", ghost_clear);
    rl_add_funmap_entry("ghostline-end", ghost_end);
    rl_add_funmap_entry("ghostline-newline", ghost_newline);
    bind_both("\\e[C", ghost_accept);
    bind_both("\\e[F", ghost_end);
    bind_both("\\eOF", ghost_end);
    bind_both("\\C-f", ghost_accept);
    bind_both("\\e\\e[C", ghost_accept_word);
    bind_both("\\C-]", ghost_clear);
    /* clear-widgets parity: save orig before overriding */
    bind_wrap_both("\\C-i", ghost_tab, &o_tab);
    bind_wrap_both("\\ef", ghost_accept_word, &o_altf);
    bind_wrap_both("\\C-m", ghost_newline, &o_accept_m);
    bind_wrap_both("\\C-j", ghost_newline, &o_accept_j);
    bind_wrap_both("\\e[D", ghost_left, &o_left);
    bind_wrap_both("\\e[H", ghost_home, &o_home);
    bind_wrap_both("\\e[1~", ghost_home1, &o_home1);
    bind_wrap_both("\\C-a", ghost_beg, &o_beg);
    bind_wrap_both("\\C-b", ghost_back, &o_back);
    {
        rl_command_func_t *pb = saved_func("\\e[200~");
        if (pb) { o_paste = pb; bind_both("\\e[200~", ghost_paste); }
    }
    resolve_bin();
    /* Init size tracking so first redisplay after enable doesn't
     * false-trigger the SIGWINCH clear. */
    {
        int rows = 0, cols = 0;
        rl_get_screen_size(&rows, &cols);
        g_last_rows = rows;
        g_last_cols = cols;
    }
    g_enabled = 1;
    return 0;
}

static int do_disable(void) {
    g_enabled = 0;
    if (orig_redisplay) rl_redisplay_function = orig_redisplay;
    free_str(&g_cached_full);
    drop_painted();
    g_last_rows = -1;
    g_last_cols = -1;
    return 0;
}

static int ghostline_autosuggest_builtin(WORD_LIST *list) {
    const char *arg = (list && list->word) ? list->word->word : "enable";
    if (strcmp(arg, "enable") == 0) { do_enable(); return EXECUTION_SUCCESS; }
    if (strcmp(arg, "disable") == 0) { do_disable(); return EXECUTION_SUCCESS; }
    if (strcmp(arg, "status") == 0) {
        printf("ghostline-autosuggest %s\n", g_enabled ? "enabled" : "disabled");
        return EXECUTION_SUCCESS;
    }
    builtin_error("usage: ghostline_autosuggest [enable|disable|status]");
    return EX_USAGE;
}

int ghostline_autosuggest_unload(void) {
    do_disable();
    orig_redisplay = NULL;
    o_accept_m = o_accept_j = o_left = o_home = o_home1 = NULL;
    o_beg = o_back = o_paste = o_tab = o_altf = NULL;
    return 0;
}

static char *ghostline_autosuggest_doc[] = {
    "Flicker-free auto-ghost via readline redisplay hook.",
    "Usage: ghostline_autosuggest [enable|disable|status]",
    (char *)NULL
};

struct builtin ghostline_autosuggest_struct = {
    "ghostline_autosuggest",
    ghostline_autosuggest_builtin,
    BUILTIN_ENABLED,
    ghostline_autosuggest_doc,
    "ghostline_autosuggest [enable|disable|status]",
    0
};
