use std::fs;

#[test]
fn macos_info_plist_has_minimal_app_bundle_metadata() {
    let plist = fs::read_to_string("packaging/macos/Info.plist").expect("read Info.plist");

    assert!(plist.contains("<key>CFBundleExecutable</key>"));
    assert!(plist.contains("<string>Markview</string>"));
    assert!(plist.contains("<key>CFBundleIdentifier</key>"));
    assert!(plist.contains("<string>com.baldwinmatt.markview</string>"));
    assert!(plist.contains("<key>CFBundleIconFile</key>"));
    assert!(plist.contains("<key>CFBundlePackageType</key>"));
    assert!(plist.contains("<string>APPL</string>"));
    assert!(plist.contains("<key>CFBundleDocumentTypes</key>"));
    assert!(plist.contains("<string>Markdown Document</string>"));
    assert!(plist.contains("<string>md</string>"));
    assert!(plist.contains("<string>markdown</string>"));
    assert!(plist.contains("<string>mdown</string>"));
    assert!(plist.contains("<string>net.daringfireball.markdown</string>"));
    assert!(plist.contains("<string>Viewer</string>"));
}

#[test]
fn macos_bundle_script_keeps_cargo_as_the_build_path() {
    let script = fs::read_to_string("packaging/macos/bundle.sh").expect("read bundle script");

    assert!(script.contains("cargo build"));
    assert!(script.contains("--features \"${FEATURES}\""));
    assert!(script.contains("--bin \"${BINARY_NAME}\""));
    assert!(script.contains("target/macos/${APP_NAME}.app"));
    assert!(script.contains("Contents/MacOS/${APP_NAME}"));
    assert!(script.contains("Markview.icns.base64"));
    assert!(script.contains("Contents/Resources/Markview.icns"));
}

#[test]
fn macos_package_script_creates_versioned_release_zip() {
    let script = fs::read_to_string("packaging/macos/package.sh").expect("read package script");

    assert!(script.contains("BUILD_MODE=release"));
    assert!(script.contains("packaging/macos/bundle.sh"));
    assert!(script.contains("target/dist"));
    assert!(script.contains("markview-${VERSION}-macos.zip"));
    assert!(script.contains("ditto -c -k --keepParent"));
}

#[test]
fn makefile_exposes_packaging_command() {
    let makefile = fs::read_to_string("Makefile").expect("read Makefile");

    assert!(makefile.contains("package-macos:"));
    assert!(makefile.contains("sh packaging/macos/package.sh"));
}

#[test]
fn release_notes_cover_packaged_release() {
    let notes = fs::read_to_string("RELEASE_NOTES.md").expect("read release notes");

    assert!(notes.contains("## 0.1.0"));
    assert!(notes.contains("macOS `.app` bundle"));
    assert!(notes.contains("repeatable zip packaging command"));
}

#[test]
fn macos_icon_payload_is_an_icns_file() {
    let encoded = fs::read_to_string("packaging/macos/Markview.icns.base64").expect("read icon");
    let decoded = decode_base64(encoded.trim()).expect("decode icon");

    assert!(decoded.starts_with(b"icns"));
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;

    for byte in input.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        if byte == b'=' {
            break;
        }
        let value = ALPHABET.iter().position(|candidate| *candidate == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }

    Some(output)
}
