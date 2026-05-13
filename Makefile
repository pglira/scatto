PREFIX ?= $(HOME)/.local
BIN    ?= scatto

.PHONY: build release install uninstall clean

build:
	cargo build

release:
	cargo build --release

install: release
	install -Dm755 target/release/$(BIN) $(PREFIX)/bin/$(BIN)

uninstall:
	rm -f $(PREFIX)/bin/$(BIN)

clean:
	cargo clean
