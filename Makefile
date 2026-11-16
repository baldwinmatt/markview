.PHONY: test test-browser package-macos

test:
	cargo test
	cargo test --features gui
	node --test tests/editor.test.cjs
	node --test tests/review.test.cjs

# Runs tests/review.browser.js in headless Chrome or Chromium, and fails
# instead of skipping when neither is installed.
test-browser:
	MARKVIEW_REQUIRE_BROWSER=1 cargo test --bin markview review_browser_harness -- --nocapture

package-macos:
	sh packaging/macos/package.sh
