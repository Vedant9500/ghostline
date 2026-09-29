# builtin/Makefile — native auto-ghost loadable (bash headers + cc required).
SO ?= ghostline_autosuggest.so
SRC := ghostline_autosuggest.c
CFLAGS ?= -shared -fPIC -O2 -Wall -Wextra
INCS := -I/usr/include/bash -I/usr/include/bash/include

all: $(SO)

$(SO): $(SRC)
	cc $(CFLAGS) -o $@ $(SRC) $(INCS)

clean:
	rm -f $(SO)

.PHONY: all clean
