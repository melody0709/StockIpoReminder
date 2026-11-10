use std::borrow::Cow;

use crate::finance::PaymentAccountKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BankDefinition {
    pub id: &'static str,
    pub display_name: &'static str,
    pub aliases: &'static [&'static str],
}

/// 稳定的内部银行标识。顺序只用于展示，不参与持久化。
pub(crate) const BANK_REGISTRY: &[BankDefinition] = &[
    BankDefinition {
        id: "icbc",
        display_name: "工商银行",
        aliases: &["中国工商银行", "工行", "工银", "ICBC"],
    },
    BankDefinition {
        id: "abc",
        display_name: "农业银行",
        aliases: &["中国农业银行", "农行", "农银", "ABC"],
    },
    BankDefinition {
        id: "boc",
        display_name: "中国银行",
        aliases: &["中行", "中银", "BOC"],
    },
    BankDefinition {
        id: "ccb",
        display_name: "建设银行",
        aliases: &["中国建设银行", "建行", "建融", "建银", "CCB"],
    },
    BankDefinition {
        id: "bankcomm",
        display_name: "交通银行",
        aliases: &["交行", "交银", "BCM", "BOCOM"],
    },
    BankDefinition {
        id: "psbc",
        display_name: "邮储银行",
        aliases: &[
            "中国邮政储蓄银行",
            "邮政储蓄银行",
            "邮储",
            "邮储银",
            "邮政",
            "PSBC",
        ],
    },
    BankDefinition {
        id: "cmb",
        display_name: "招商银行",
        aliases: &["招行", "招银", "CMB"],
    },
    BankDefinition {
        id: "citic",
        display_name: "中信银行",
        aliases: &["中信", "CITIC"],
    },
    BankDefinition {
        id: "spdb",
        display_name: "浦发银行",
        aliases: &["上海浦东发展银行", "浦发", "浦银", "SPDB"],
    },
    BankDefinition {
        id: "cmbc",
        display_name: "民生银行",
        aliases: &["中国民生银行", "民生", "民银", "CMBC"],
    },
    BankDefinition {
        id: "cib",
        display_name: "兴业银行",
        aliases: &["兴业", "兴银", "CIB"],
    },
    BankDefinition {
        id: "ceb",
        display_name: "光大银行",
        aliases: &["中国光大银行", "光大", "CEB"],
    },
    BankDefinition {
        id: "hxb",
        display_name: "华夏银行",
        aliases: &["华夏", "HXB"],
    },
    BankDefinition {
        id: "pingan",
        display_name: "平安银行",
        aliases: &["平安", "PINGAN", "PAB"],
    },
    BankDefinition {
        id: "cgb",
        display_name: "广发银行",
        aliases: &["广东发展银行", "广发", "CGB", "GDB"],
    },
    BankDefinition {
        id: "bob",
        display_name: "北京银行",
        aliases: &["BOB"],
    },
    BankDefinition {
        id: "bos",
        display_name: "上海银行",
        aliases: &["BOS", "SHB"],
    },
    BankDefinition {
        id: "jsbank",
        display_name: "江苏银行",
        aliases: &["JSB", "JSBC"],
    },
    BankDefinition {
        id: "njcb",
        display_name: "南京银行",
        aliases: &["NJCB"],
    },
    BankDefinition {
        id: "nbcb",
        display_name: "宁波银行",
        aliases: &["NBCB"],
    },
    BankDefinition {
        id: "czbank",
        display_name: "浙商银行",
        aliases: &["CZB", "CZBANK"],
    },
    BankDefinition {
        id: "other",
        display_name: "其他银行",
        aliases: &["其它银行"],
    },
];

pub(crate) fn bank_definition(id: &str) -> Option<&'static BankDefinition> {
    BANK_REGISTRY.iter().find(|bank| bank.id == id)
}

fn escape_xml_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// 针对自定义银行名称，按前两个字符及名称确定性哈希生成专属颜色徽标
pub(crate) fn generate_monogram_svg(name: &str) -> Vec<u8> {
    let clean = name.trim();
    let mut display_text: String = clean
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .collect();
    if display_text.is_empty() {
        display_text = clean.chars().take(2).collect();
    }
    let display_text = escape_xml_text(&display_text);
    const PALETTE: &[&str] = &[
        "#2563eb", "#0284c7", "#0d9488", "#059669", "#16a34a", "#7c3aed", "#9333ea", "#c026d3",
        "#e11d48", "#ea580c",
    ];
    let mut hash: usize = 0;
    for b in clean.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(b as usize);
    }
    let bg_color = PALETTE[hash % PALETTE.len()];
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96"><rect width="96" height="96" rx="20" fill="{bg_color}"/><text x="48" y="58" text-anchor="middle" font-family="Microsoft YaHei,Segoe UI,sans-serif" font-size="28" font-weight="bold" fill="#ffffff">{display_text}</text></svg>"##,
        bg_color = bg_color,
        display_text = display_text
    )
    .into_bytes()
}

pub(crate) fn account_icon_svg(
    id: Option<&str>,
    kind: PaymentAccountKind,
    custom_name: Option<&str>,
) -> Cow<'static, [u8]> {
    match id {
        Some("icbc") => Cow::Borrowed(include_bytes!("../../assets/banks/icbc.svg")),
        Some("abc") => Cow::Borrowed(include_bytes!("../../assets/banks/abc.svg")),
        Some("boc") => Cow::Borrowed(include_bytes!("../../assets/banks/boc.svg")),
        Some("ccb") => Cow::Borrowed(include_bytes!("../../assets/banks/ccb.svg")),
        Some("bankcomm") => Cow::Borrowed(include_bytes!("../../assets/banks/bankcomm.svg")),
        Some("psbc") => Cow::Borrowed(include_bytes!("../../assets/banks/psbc.svg")),
        Some("cmb") => Cow::Borrowed(include_bytes!("../../assets/banks/cmb.svg")),
        Some("citic") => Cow::Borrowed(include_bytes!("../../assets/banks/citic.svg")),
        Some("spdb") => Cow::Borrowed(include_bytes!("../../assets/banks/spdb.svg")),
        Some("cmbc") => Cow::Borrowed(include_bytes!("../../assets/banks/cmbc.svg")),
        Some("cib") => Cow::Borrowed(include_bytes!("../../assets/banks/cib.svg")),
        Some("ceb") => Cow::Borrowed(include_bytes!("../../assets/banks/ceb.svg")),
        Some("hxb") => Cow::Borrowed(include_bytes!("../../assets/banks/hxb.svg")),
        Some("pingan") => Cow::Borrowed(include_bytes!("../../assets/banks/pingan.svg")),
        Some("cgb") => Cow::Borrowed(include_bytes!("../../assets/banks/cgb.svg")),
        Some("bob") => Cow::Borrowed(include_bytes!("../../assets/banks/bob.svg")),
        Some("bos") => Cow::Borrowed(include_bytes!("../../assets/banks/bos.svg")),
        Some("jsbank") => Cow::Borrowed(include_bytes!("../../assets/banks/jsbank.svg")),
        Some("njcb") => Cow::Borrowed(include_bytes!("../../assets/banks/njcb.svg")),
        Some("nbcb") => Cow::Borrowed(include_bytes!("../../assets/banks/nbcb.svg")),
        Some("czbank") => Cow::Borrowed(include_bytes!("../../assets/banks/czbank.svg")),
        Some("other") => {
            if let Some(name) = custom_name.map(str::trim).filter(|n| !n.is_empty()) {
                Cow::Owned(generate_monogram_svg(name))
            } else {
                Cow::Borrowed(include_bytes!("../../assets/banks/bank_building.svg"))
            }
        }
        None => match kind {
            PaymentAccountKind::CreditCard => {
                Cow::Borrowed(include_bytes!("../../assets/banks/kind_credit_card.svg"))
            }
            PaymentAccountKind::Mortgage => {
                Cow::Borrowed(include_bytes!("../../assets/banks/kind_mortgage.svg"))
            }
            PaymentAccountKind::AutoLoan => {
                Cow::Borrowed(include_bytes!("../../assets/banks/kind_auto_loan.svg"))
            }
            PaymentAccountKind::CreditLoan => {
                Cow::Borrowed(include_bytes!("../../assets/banks/kind_credit_loan.svg"))
            }
        },
        _ => Cow::Borrowed(include_bytes!("../../assets/banks/bank_building.svg")),
    }
}

pub(crate) fn account_icon(
    id: Option<&str>,
    kind: PaymentAccountKind,
    custom_name: Option<&str>,
) -> slint::Image {
    let bytes = account_icon_svg(id, kind, custom_name);
    slint::Image::load_from_svg_data(&bytes).unwrap_or_default()
}

#[allow(dead_code)]
pub(crate) fn bank_icon_svg(id: Option<&str>) -> Cow<'static, [u8]> {
    account_icon_svg(id, PaymentAccountKind::CreditCard, None)
}

pub(crate) fn bank_icon(id: Option<&str>) -> slint::Image {
    account_icon(id, PaymentAccountKind::CreditCard, None)
}

fn normalize_bank_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 仅做登记名称的精确匹配；不猜测未登记的相似名称。
pub(crate) fn bank_id_for_name(value: &str) -> Option<&'static str> {
    let normalized = normalize_bank_name(value);
    if normalized.is_empty() {
        return None;
    }
    BANK_REGISTRY
        .iter()
        .find(|bank| {
            normalize_bank_name(bank.display_name) == normalized
                || bank
                    .aliases
                    .iter()
                    .any(|alias| normalize_bank_name(alias) == normalized)
        })
        .map(|bank| bank.id)
}

pub(crate) fn legacy_bank_id(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        None
    } else {
        Some(bank_id_for_name(value).unwrap_or("other").to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn registry_has_stable_unique_ids_and_all_planned_banks() {
        assert_eq!(BANK_REGISTRY.len(), 22);
        let ids = BANK_REGISTRY
            .iter()
            .map(|bank| bank.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), 22);
        for id in [
            "icbc", "abc", "boc", "ccb", "bankcomm", "psbc", "cmb", "citic", "spdb", "cmbc", "cib",
            "ceb", "hxb", "pingan", "cgb", "bob", "bos", "jsbank", "njcb", "nbcb", "czbank",
            "other",
        ] {
            assert!(ids.contains(id), "缺少银行 ID：{id}");
        }
    }

    #[test]
    fn legacy_names_use_only_registered_exact_aliases() {
        assert_eq!(legacy_bank_id(Some(" 招行 ")).as_deref(), Some("cmb"));
        assert_eq!(legacy_bank_id(Some("招银")).as_deref(), Some("cmb"));
        assert_eq!(
            legacy_bank_id(Some("中国 工商银行")).as_deref(),
            Some("icbc")
        );
        assert_eq!(legacy_bank_id(Some("工银")).as_deref(), Some("icbc"));
        assert_eq!(legacy_bank_id(Some("建融")).as_deref(), Some("ccb"));
        assert_eq!(legacy_bank_id(Some("交银")).as_deref(), Some("bankcomm"));
        assert_eq!(legacy_bank_id(Some("icbc")).as_deref(), Some("icbc"));
        assert_eq!(legacy_bank_id(Some("招商")).as_deref(), Some("other"));
        assert_eq!(
            legacy_bank_id(Some("未知地方银行")).as_deref(),
            Some("other")
        );
        assert_eq!(legacy_bank_id(Some("  ")), None);
        assert_eq!(legacy_bank_id(None), None);
    }

    #[test]
    fn account_icon_svg_returns_semantic_fallbacks_for_unspecified_bank() {
        let card = account_icon_svg(None, PaymentAccountKind::CreditCard, None);
        assert!(card.starts_with(b"<svg"));
        assert!(String::from_utf8_lossy(&card).contains("#2563eb"));

        let mortgage = account_icon_svg(None, PaymentAccountKind::Mortgage, None);
        assert!(mortgage.starts_with(b"<svg"));
        assert!(String::from_utf8_lossy(&mortgage).contains("#0d9488"));

        let auto_loan = account_icon_svg(None, PaymentAccountKind::AutoLoan, None);
        assert!(auto_loan.starts_with(b"<svg"));
        assert!(String::from_utf8_lossy(&auto_loan).contains("#7c3aed"));
    }

    #[test]
    fn account_icon_svg_generates_deterministic_monogram_for_custom_bank() {
        let monogram1 = account_icon_svg(
            Some("other"),
            PaymentAccountKind::CreditCard,
            Some("盛京银行"),
        );
        let monogram2 = account_icon_svg(
            Some("other"),
            PaymentAccountKind::CreditCard,
            Some("盛京银行"),
        );
        assert_eq!(monogram1, monogram2);
        let s = String::from_utf8_lossy(&monogram1);
        assert!(s.contains("盛京"));

        let bracketed = account_icon_svg(
            Some("other"),
            PaymentAccountKind::CreditCard,
            Some("（渣打）银行"),
        );
        let s_bracketed = String::from_utf8_lossy(&bracketed);
        assert!(s_bracketed.contains("渣打"));

        let empty_custom =
            account_icon_svg(Some("other"), PaymentAccountKind::CreditCard, Some("  "));
        let building = String::from_utf8_lossy(&empty_custom);
        assert!(building.contains("polygon"));
    }

    #[test]
    fn no_duplicate_aliases_in_bank_registry() {
        let mut seen = std::collections::HashMap::new();
        for bank in BANK_REGISTRY {
            for alias in bank.aliases {
                let normalized = normalize_bank_name(alias);
                if let Some(existing_bank) = seen.insert(normalized.clone(), bank.id) {
                    panic!(
                        "Duplicate alias detected: '{}' belongs to both {} and {}",
                        alias, existing_bank, bank.id
                    );
                }
            }
        }
    }

    #[test]
    fn all_bank_icons_and_fallbacks_produce_valid_renderable_images() {
        for bank in BANK_REGISTRY {
            let img = account_icon(Some(bank.id), PaymentAccountKind::CreditCard, None);
            let size = img.size();
            assert!(
                size.width > 0 && size.height > 0,
                "Bank {} failed to render: {:?}",
                bank.id,
                size
            );
        }

        for kind in [
            PaymentAccountKind::CreditCard,
            PaymentAccountKind::Mortgage,
            PaymentAccountKind::AutoLoan,
            PaymentAccountKind::CreditLoan,
        ] {
            let img = account_icon(None, kind, None);
            let size = img.size();
            assert!(
                size.width > 0 && size.height > 0,
                "Semantic fallback {:?} failed to render: {:?}",
                kind,
                size
            );
        }

        let custom_img = account_icon(
            Some("other"),
            PaymentAccountKind::CreditCard,
            Some("AT&T <Bank>"),
        );
        let size = custom_img.size();
        assert!(
            size.width > 0 && size.height > 0,
            "Custom monogram with XML special chars failed to render: {:?}",
            size
        );
    }
}
