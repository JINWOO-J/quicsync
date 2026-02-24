.PHONY: build release test check clean install release-all dist

UNAME_S := $(shell uname -s)

# macOS에서 Linux 타겟은 cross 필요, Linux에서는 cargo로 직접 빌드
ifeq ($(UNAME_S),Darwin)
  CROSS_CMD = cross
else
  CROSS_CMD = cargo
endif

LINUX_TARGETS = x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
DIST_DIR = dist

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
release-x86-mac:
	cargo build --release --target x86_64-apple-darwin

release-x86-linux:
	$(CROSS_CMD) build --release --target x86_64-unknown-linux-gnu

release-arm-linux:
	$(CROSS_CMD) build --release --target aarch64-unknown-linux-gnu

# 전체 릴리스: native + 가능한 cross 타겟
release-all: release
ifeq ($(UNAME_S),Darwin)
	@# macOS: x86 mac은 rustup, Linux 타겟은 cross
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

# 빌드 결과를 dist/에 모아서 배포용으로 정리
dist: release-all
	mkdir -p $(DIST_DIR)
	@cp -f target/release/quicsync $(DIST_DIR)/quicsync-$(UNAME_S)-native 2>/dev/null || true
	@for target in x86_64-apple-darwin $(LINUX_TARGETS); do \
		if [ -f target/$$target/release/quicsync ]; then \
			cp target/$$target/release/quicsync $(DIST_DIR)/quicsync-$$target; \
			echo "Copied quicsync-$$target"; \
		fi; \
	done
	@echo "Artifacts in $(DIST_DIR)/:"
	@ls -lh $(DIST_DIR)/
