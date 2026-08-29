use super::*;

#[test]
fn xml_escape_keeps_xml_10_chars_and_filters_illegal_control_bytes() {
    assert_eq!(xml_escape("a\tb\nc\rd"), "a\tb\nc\rd");
    assert_eq!(
        xml_escape("<a>&\"'</a>"),
        "&lt;a&gt;&amp;&quot;&apos;&lt;/a&gt;"
    );
    assert_eq!(xml_escape("中\u{0}文\u{8}"), "中文");
    assert_eq!(xml_escape("emoji \u{1F600}"), "emoji \u{1F600}");
    assert_eq!(xml_escape(""), "");
}

#[test]
fn activation_message_is_stable_and_scoped_to_the_data_root() {
    let first = activation_message_name(Path::new("C:\\Data\\StockIpoReminder"));
    let same = activation_message_name(Path::new("c:\\data\\stockiporeminder"));
    let other = activation_message_name(Path::new("D:\\Data\\StockIpoReminder"));
    assert_eq!(first, same);
    assert_ne!(first, other);
    assert!(!first.contains("C:\\Data"));
}

#[test]
fn toast_xml_escapes_untrusted_text_and_limits_payload_size() {
    let xml = toast_xml(
        "A&B <测试>",
        &format!("'\"{}", "字".repeat(600)),
        Some("shanghai:601001&version=2"),
    );
    assert!(xml.contains("A&amp;B &lt;测试&gt;"));
    assert!(xml.contains("&apos;&quot;"));
    assert!(!xml.contains("A&B"));
    assert!(xml.chars().count() < 800);
    assert!(xml.contains('…'));
    assert!(xml.contains("launch=\"eventId=shanghai:601001&amp;version=2\""));
}
