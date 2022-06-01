OS=$(shell uname)
ifeq ($(OS),Darwin)
  SNAPDIR_BIN_FILES=$(shell find . -maxdepth 1 -type f -perm +0111 -print | sed 's|^\./||')
else
  SNAPDIR_BIN_FILES=$(shell find . -maxdepth 1 -perm /u=x,g=x,o=x  -type f | sed 's|^\./||')
endif

API_DOC_TARGETS=$(patsubst %, docs/api/%.md, $(SNAPDIR_BIN_FILES))

test:
	./snapdir-test

.PHONY: docs
docs: $(API_DOC_TARGETS)

pre-commit: docs
	SNAPDIR_BIN_FILES="$(SNAPDIR_BIN_FILES)" ./utils/pre-commit-hook.sh

pre-push:
	./utils/verify-docs.sh

docs/api/%.md: %
	echo "Generating API docs for $*"
	DEBUG=false ./utils/generate-docs.sh $* > docs/api/$*.md

utils/qa-fixtures/tested-commands.sh: $(SNAPDIR_BIN_FILES)
	_SNAPDIR_RUN_LOG_PATH="$$(pwd)/utils/qa-fixtures/tested-commands.sh" ./snapdir-test integration

build: docs

install:
	@INSTALL_DIR="$(DESTDIR)$(PREFIX)" && \
	INSTALL_DIR="$${INSTALL_DIR:-/usr/local/bin/}" && \
	mkdir -p "$${INSTALL_DIR}" && \
	cp snapdir* "$${INSTALL_DIR%/}/"
	@command -v snapdir
	@snapdir -v

install-linked:
	@INSTALL_DIR="$(DESTDIR)$(PREFIX)" && \
	INSTALL_DIR="$${INSTALL_DIR:-/usr/local/bin/}" && \
	mkdir -p "$${INSTALL_DIR}" && \
	for bin_file in $(SNAPDIR_BIN_FILES); do \
		rm -f "$${INSTALL_DIR%/}/$$(basename $$bin_file)" && \
		ln -s "$$(pwd)/$$bin_file" "$${INSTALL_DIR%/}/$$(basename $$bin_file)"; \
	done
	@command -v snapdir
	@snapdir -v
