.PHONY: build release test check clean install release-all dist dist-bump bump

UNAME_S := $(shell uname -s)

# macOS에서 Linux 타겟은 cross 필요, Linux에서는 cargo로 직접 빌드
ifeq ($(UNAME_S),Darwin)
  CROSS_CMD = cross
else
  CROSS_CMD = cargo
endif

LINUX_TARGETS = x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
DIST_DIR = dist
DEPLOY_SERVER := jwserver68
SERVER_USER := root

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

check:
	cargo check
	cargo clippy -- -D warnings

clean:
	cargo clean
	rm -rf $(DIST_DIR)

# OS별 설치 경로: Linux → /usr/local/bin, macOS → ~/.local/bin
install: release
ifeq ($(UNAME_S),Linux)
	install -Dm755 target/release/quicsync /usr/local/bin/quicsync
else
	mkdir -p ~/.local/bin
	cp target/release/quicsync ~/.local/bin/
endif

# 개별 타겟 빌드
release-arm-mac:
	cargo build --release --target aarch64-apple-darwin

release-x86-mac:
	cargo build --release --target x86_64-apple-darwin

release-x86-linux:
	$(CROSS_CMD) build --release --target x86_64-unknown-linux-gnu

release-arm-linux:
	$(CROSS_CMD) build --release --target aarch64-unknown-linux-gnu

# 전체 릴리스: native + 가능한 cross 타겟
release-all: release
ifeq ($(UNAME_S),Darwin)
	@# macOS: x86/arm mac은 rustup, Linux 타겟은 cross
	@if rustup target list --installed | grep -q aarch64-apple-darwin; then \
		echo "Building aarch64-apple-darwin..."; \
		cargo build --release --target aarch64-apple-darwin; \
	fi
	@if rustup target list --installed | grep -q x86_64-apple-darwin; then \
		echo "Building x86_64-apple-darwin..."; \
		cargo build --release --target x86_64-apple-darwin; \
	fi
	@if command -v cross >/dev/null 2>&1; then \
		for target in $(LINUX_TARGETS); do \
			echo "Building $$target (via cross)..."; \
			cross build --release --target "$$target"; \
		done; \
	else \
		echo "Skipping Linux targets (install cross: cargo install cross)"; \
	fi
else
	@# Linux: 같은 OS 타겟은 cargo로 직접
	@for target in $(LINUX_TARGETS); do \
		if rustup target list --installed | grep -q "$$target"; then \
			echo "Building $$target..."; \
			cargo build --release --target "$$target"; \
		else \
			echo "Skipping $$target (run: rustup target add $$target)"; \
		fi; \
	done
endif

# patch 버전 +1 (0.1.0 → 0.1.1)
bump:
	@CURRENT=$$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'); \
	MAJOR=$$(echo $$CURRENT | cut -d. -f1); \
	MINOR=$$(echo $$CURRENT | cut -d. -f2); \
	PATCH=$$(echo $$CURRENT | cut -d. -f3); \
	NEW="$$MAJOR.$$MINOR.$$((PATCH + 1))"; \
	sed -i '' "s/^version = \"$$CURRENT\"/version = \"$$NEW\"/" Cargo.toml; \
	echo "$$CURRENT → $$NEW"

# 빌드 결과를 dist/에 모아서 배포용으로 정리
dist: release-all
	mkdir -p $(DIST_DIR)
	@cp -f target/release/quicsync $(DIST_DIR)/quicsync-$(UNAME_S)-native 2>/dev/null || true
	@for target in aarch64-apple-darwin x86_64-apple-darwin $(LINUX_TARGETS); do \
		if [ -f target/$$target/release/quicsync ]; then \
			cp target/$$target/release/quicsync $(DIST_DIR)/quicsync-$$target; \
			echo "Copied quicsync-$$target"; \
		fi; \
	done
	@rm -f $(DIST_DIR)/quicsync_*.tar.gz $(DIST_DIR)/checksums.txt
	@package() { \
		src="$$1"; asset="$$2"; \
		if [ -f "$$src" ]; then \
			tmp=$$(mktemp -d); \
			cp "$$src" "$$tmp/quicsync"; \
			tar -C "$$tmp" -czf "$(DIST_DIR)/$$asset" quicsync; \
			rm -rf "$$tmp"; \
			echo "Packaged $$asset"; \
		fi; \
	}; \
	package target/x86_64-unknown-linux-gnu/release/quicsync quicsync_linux_x86_64.tar.gz; \
	package target/aarch64-unknown-linux-gnu/release/quicsync quicsync_linux_aarch64.tar.gz; \
	package target/x86_64-apple-darwin/release/quicsync quicsync_macos_x86_64.tar.gz; \
	if [ "$(UNAME_S)" = "Darwin" ]; then \
		arch=$$(uname -m); \
		package target/release/quicsync quicsync_macos_$$arch.tar.gz; \
	fi
	@if ls $(DIST_DIR)/quicsync_*.tar.gz >/dev/null 2>&1; then \
		(cd $(DIST_DIR) && shasum -a 256 quicsync_*.tar.gz > checksums.txt); \
		echo "Wrote $(DIST_DIR)/checksums.txt"; \
	fi
	@echo "Artifacts in $(DIST_DIR)/:"
	@ls -lh $(DIST_DIR)/

dist-bump: bump dist

deploy: dist
	scp -p target/x86_64-unknown-linux-gnu/release/quicsync $(SERVER_USER)@$(DEPLOY_SERVER):/usr/local/bin/
