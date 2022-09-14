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
	printf "New version: " && \
	read -r NEW_VERSION && \
	echo "New version: $$NEW_VERSION" && \
	sed -i '' "s/$$CURRENT_VERSION/$$NEW_VERSION/g" snapdir && \
	sed -i '' "s/$$CURRENT_VERSION/$$NEW_VERSION/g" snapdir-manifest && \
	make docs && \
	git add ./snapdir ./snapdir-manifest ./docs/api/ && \
	git commit -m "Bumping version to $$NEW_VERSION" && \
	git tag -a v$$NEW_VERSION -m "Release $$NEW_VERSION" && \
	git push && \
	git push origin v$$NEW_VERSION


website-dev:
	command -v retype 2>&1 >/dev/null || npm install retypeapp --global
	cd docs && retype watch

.PHONY: publish-website
publish-website:
	: $${GCP_PROJECT:?"Missing GCP_PROJECT"}
	rm -rf tmp/website tmp/website-docs
	mkdir -p tmp
	cp -R docs tmp/website-docs
	find tmp/website-docs/api/ -type f -exec sed -i '' 's|docs/|../|g' {} \;
	command -v retype 2>&1 >/dev/null || npm install retypeapp --global
	cd tmp/website-docs && retype build
	cp utils/website.dockerfile tmp/website/Dockerfile
	cp docs/images/favicon.ico tmp/website/favicon.ico
	cd tmp/website && \
		gcloud builds submit --tag gcr.io/$${GCP_PROJECT}/tmp/website-snapdir && \
		gcloud run deploy --image gcr.io/$${GCP_PROJECT}/tmp/website-snapdir
	rm -rf tmp/website tmp/website-docs