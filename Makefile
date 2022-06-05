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

lint:
	echo "$(SNAPDIR_BIN_FILES)" | xargs shellcheck
	echo "$(SNAPDIR_BIN_FILES)" | xargs shfmt -w -s

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

release:
	@CURRENT_VERSION="$$(./snapdir --version)" && \
	echo "Current version: $$CURRENT_VERSION" && \
	echo -n "New version: " && \
	read NEW_VERSION && \
	echo "New version: $$NEW_VERSION" && \
	sed -i "s/$$CURRENT_VERSION/$$NEW_VERSION/" ./snapdir && \
	sed -i "s/$$CURRENT_VERSION/$$NEW_VERSION/" ./snapdir-manifest && \
	make docs && \
	git add ./snapdir ./snapdir-manifest ./docs/api/ && \
	git commit -m "Bumping version to $$NEW_VERSION" && \
	git tag -a v$$NEW_VERSION -m "Release $$NEW_VERSION" && \
	git push main && \
	git push origin v$$NEW_VERSION