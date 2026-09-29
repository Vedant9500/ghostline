/* ghostline_autosuggest.c — Bash loadable builtin for flicker-free auto-ghost.
 *
 * Hooks readline's own redisplay (rl_redisplay_function) so the ghost suffix
 * is drawn AS PART OF the normal redisplay, not as a second full-line
 * `bind -x` refresh. Typing stays native/incremental; only the ghost region
 * (after cursor) is painted per key. No `bind -x` printable hooks.
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
#include <sys/types.h>
#include <sys/wait.h>

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

static void free_str(char **p) { if (p && *p) { free(*p); *p = NULL; } }

static int has_control(const char *s) {
    for (; *s; s++) {
        unsigned char c = (unsigned char)*s;
        if (c < 0x20 || c == 0x7f) return 1;
    }
    return 0;
}

/* Fork+exec `ghostline suggest`, no shell (safe for arbitrary buffer).
 * Returns malloc'd suffix (may be empty string) or NULL on error. */
static char *fetch_suffix_fork(const char *buf, const char *cwd) {
    int fd[2];
    if (pipe(fd) != 0) return NULL;
    pid_t pid = fork();
    if (pid < 0) { close(fd[0]); close(fd[1]); return NULL; }
    if (pid == 0) {
        /* child */
        close(fd[0]);
        if (dup2(fd[1], STDOUT_FILENO) < 0) _exit(127);
        close(fd[1]);
        /* stderr silenced: ghost must never spray the prompt */
        int devnull = -1;
        /* open /dev/null without stdio (avoid malloc post-fork issues minimal) */
        devnull = open("/dev/null", 2 /* O_WRONLY */);
        if (devnull >= 0) { dup2(devnull, STDERR_FILENO); close(devnull); }
        execlp("ghostline", "ghostline", "suggest",
               "--cwd", cwd ? cwd : ".", "--", buf, (char *)NULL);
        _exit(127);
    }
    /* parent */
    close(fd[1]);
    char tmp[4096];
    size_t cap = 256, len = 0;
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
    if (!out) return NULL;
    out[len] = '\0';
    /* strip trailing newlines (println in --all; suffix print has none) */
    while (len > 0 && (out[len-1] == '\n' || out[len-1] == '\r')) out[--len] = '\0';
    return out;
}

static char *current_cwd(void) {
    static char buf[PATH_MAX];
    if (getcwd(buf, sizeof(buf))) return buf;
    return ".";
}

/* Return malloc'd suffix for buf (may be ""), using shrink-along-cache fast
 * path (no fork) when typing forward into the cached suggestion. */
static char *ghost_suffix(const char *buf) {
    size_t blen = strlen(buf);
    if (blen > 0 && blen <= 200 && g_cached_full) {
        size_t flen = strlen(g_cached_full);
        if (flen > blen && strncmp(g_cached_full, buf, blen) == 0
            && buf[blen-1] != ' ') {
            const char *s = g_cached_full + blen;
            if (!has_control(s)) return strdup(s);
        }
    }
    char *suf = fetch_suffix_fork(buf, current_cwd());
    if (!suf) return NULL;
    if (has_control(suf)) { free(suf); return strdup(""); }
    /* refresh cache: full = buf+suffix */
    free_str(&g_cached_full);
    size_t slen = strlen(suf);
    g_cached_full = malloc(blen + slen + 1);
    if (g_cached_full) {
        memcpy(g_cached_full, buf, blen);
        memcpy(g_cached_full + blen, suf, slen + 1);
    }
    return suf; /* caller frees */
}

static int usable_suffix(const char *buf, const char *suf) {
    return buf && suf && *buf && *suf && g_cached_full
        && strncmp(g_cached_full, buf, strlen(buf)) == 0;
}

static void out_write(const char *s) {
    FILE *out = rl_outstream ? rl_outstream : stdout;
    int fd = fileno(out);
    if (fd < 0) return;
    size_t n = strlen(s);
    /* single write: atomic ghost frame, no flicker */
    while (n > 0) {
        ssize_t w = write(fd, s, n);
        if (w <= 0) break;
        s += w; n -= (size_t)w;
    }
    fsync(fd);
}

static void paint_suffix(const char *suf) {
    out_write("\033[s\033[K\033[2;38;5;244m");
    out_write(suf);
    out_write("\033[m\033[u");
}

static void clear_tail(void) {
    out_write("\033[s\033[K\033[u");
}

static void ghost_redisplay(void) {
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
    if (rl_point != rl_end) {
        /* mid-line: drop cache so next EOL re-ranks; leave screen alone
         * (clearing from mid cursor would erase real line content). */
        free_str(&g_cached_full);
        free_str(&g_painted_suf);
        free_str(&g_painted_line);
        return;
    }
    const char *buf = rl_line_buffer;
    size_t blen = strlen(buf);
    if (blen == 0 || blen > 200) {
        if (g_painted_suf) { clear_tail(); free_str(&g_painted_suf); free_str(&g_painted_line); free_str(&g_cached_full); }
        return;
    }
    char *suf = ghost_suffix(buf);
    if (!suf || !*suf) {
        free(suf);
        if (g_painted_suf) { clear_tail(); }
        free_str(&g_painted_suf); free_str(&g_painted_line);
        if (!suf) free_str(&g_cached_full);
        else {
            /* empty: keep no cache (miss) so next key re-ranks */
            free_str(&g_cached_full);
        }
        return;
    }
    if (g_painted_suf && g_painted_line
        && strcmp(suf, g_painted_suf) == 0 && strcmp(buf, g_painted_line) == 0) {
        free(suf);
        return; /* already on screen */
    }
    free_str(&g_painted_suf); free_str(&g_painted_line);
    g_painted_suf = suf; /* owned */
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
                /* one shell-word: spaces then non-spaces */
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
    /* fallback: forward-word */
    rl_command_func_t *f = rl_named_function("forward-word");
    if (f) return f(1, key);
    return rl_forward_char(1, key);
}

static int ghost_clear(int count, int key) {
    (void)count; (void)key;
    free_str(&g_cached_full);
    free_str(&g_painted_suf);
    free_str(&g_painted_line);
    return 0; /* post-command redisplay + hook clears the tail */
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

static void bind_both(const char *seq, rl_command_func_t *fn) {
    rl_bind_keyseq(seq, fn);
    Keymap vim = rl_get_keymap_by_name("vi-insert");
    if (vim) rl_bind_keyseq_in_map(seq, fn, vim);
}

static int do_enable(void) {
    if (!orig_redisplay) orig_redisplay = rl_redisplay_function;
    rl_redisplay_function = ghost_redisplay;
    rl_add_funmap_entry("ghostline-accept", ghost_accept);
    rl_add_funmap_entry("ghostline-accept-word", ghost_accept_word);
    rl_add_funmap_entry("ghostline-clear", ghost_clear);
    rl_add_funmap_entry("ghostline-end", ghost_end);
    bind_both("\\e[C", ghost_accept);
    bind_both("\\e[F", ghost_end);
    bind_both("\\eOF", ghost_end);
    bind_both("\\C-f", ghost_accept);
    bind_both("\\e\\e[C", ghost_accept_word);
    bind_both("\\C-]", ghost_clear);
    g_enabled = 1;
    return 0;
}

static int do_disable(void) {
    g_enabled = 0;
    if (orig_redisplay) rl_redisplay_function = orig_redisplay;
    free_str(&g_cached_full);
    free_str(&g_painted_suf);
    free_str(&g_painted_line);
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
