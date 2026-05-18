use std::fs;

#[test]
fn macos_info_plist_has_minimal_app_bundle_metadata() {
    let plist = fs::read_to_string("packaging/macos/Info.plist").expect("read Info.plist");

    assert!(plist.contains("<key>CFBundleExecutable</key>"));
    assert!(plist.contains("<string>Markview</string>"));
    assert!(plist.contains("<key>CFBundleIdentifier</key>"));
    assert!(plist.contains("<string>com.baldwinmatt.markview</string>"));
    assert!(plist.contains("<key>CFBundlePackageType</key>"));
    assert!(plist.contains("<string>APPL</string>"));
}

#[test]
fn macos_bundle_script_keeps_cargo_as_the_build_path() {
    let script = fs::read_to_string("packaging/macos/bundle.sh").expect("read bundle script");

    assert!(script.contains("cargo build"));
    assert!(script.contains("--features \"${FEATURES}\""));
    assert!(script.contains("--bin \"${BINARY_NAME}\""));
    assert!(script.contains("target/macos/${APP_NAME}.app"));
    assert!(script.contains("Contents/MacOS/${APP_NAME}"));
}
