.PHONY: test package-macos

test:
	cargo test
	cargo test --features gui

package-macos:
	sh packaging/macos/package.sh
