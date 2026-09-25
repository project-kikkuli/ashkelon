use ashkelon::hooks::Ping;
use ashkelon::transform::ImageAttachment;

#[test]
fn same_hook_and_message_yields_same_id() {
    assert_eq!(Ping::new("lint", "boom", None).id, Ping::new("lint", "boom", None).id);
    assert_ne!(Ping::new("lint", "boom", None).id, Ping::new("lint", "bang", None).id);
    assert_ne!(Ping::new("lint", "boom", None).id, Ping::new("test", "boom", None).id);
}

#[test]
fn id_is_stable_regardless_of_fix() {
    // The fix text is presentation only; identity is hook + message.
    assert_eq!(
        Ping::new("lint", "boom", None).id,
        Ping::new("lint", "boom", Some("do this")).id
    );
}

#[test]
fn format_omits_fix_line_when_absent() {
    let p = Ping::new("lint", "bad indent", None);
    assert!(p.text.starts_with("<ashkelon-ping hook=\"lint\" id=\""));
    assert!(p.text.contains("bad indent"));
    assert!(!p.text.contains("fix:"));
    assert!(p.text.ends_with("</ashkelon-ping>"));
}

#[test]
fn format_includes_fix_line_when_present() {
    let p = Ping::new("lint", "bad indent", Some("run fmt"));
    assert!(p.text.contains("fix: run fmt\n</ashkelon-ping>"));
}

#[test]
fn hook_name_is_preserved() {
    assert_eq!(Ping::new("my-hook", "msg", None).hook, "my-hook");
}

#[test]
fn image_signal_identity_includes_alt_text() {
    let image = |alt: &str| ImageAttachment {
        mime_type: "image/png".into(),
        data_base64: "aW1hZ2U=".into(),
        alt_text: Some(alt.into()),
    };
    let a = Ping::signal("agentanyl", "", vec![image("image reference A")]);
    let same = Ping::signal("agentanyl", "", vec![image("image reference A")]);
    let changed = Ping::signal("agentanyl", "", vec![image("image reference B")]);
    assert_eq!(a.id, same.id);
    assert_ne!(a.id, changed.id);
}
