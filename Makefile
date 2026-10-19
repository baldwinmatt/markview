.PHONY: test package-macos

test:
	cargo test
	cargo test --features gui
	node --test tests/editor.test.cjs

package-macos:
	sh packaging/macos/package.sh
