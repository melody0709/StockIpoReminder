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
        aliases: &["中国工商银行", "工行", "ICBC"],
    },
    BankDefinition {
        id: "abc",
        display_name: "农业银行",
        aliases: &["中国农业银行", "农行", "ABC"],
    },
    BankDefinition {
        id: "boc",
        display_name: "中国银行",
        aliases: &["中行", "BOC"],
    },
    BankDefinition {
        id: "ccb",
        display_name: "建设银行",
        aliases: &["中国建设银行", "建行", "CCB"],
    },
    BankDefinition {
        id: "bankcomm",
        display_name: "交通银行",
        aliases: &["交行", "BCM"],
    },
    BankDefinition {
        id: "psbc",
        display_name: "邮储银行",
        aliases: &["中国邮政储蓄银行", "邮政储蓄银行", "邮储", "PSBC"],
    },
    BankDefinition {
        id: "cmb",
        display_name: "招商银行",
        aliases: &["招行", "CMB"],
    },
    BankDefinition {
        id: "citic",
        display_name: "中信银行",
        aliases: &["中信", "CITIC"],
    },
    BankDefinition {
        id: "spdb",
        display_name: "浦发银行",
        aliases: &["上海浦东发展银行", "浦发", "SPDB"],
    },
    BankDefinition {
        id: "cmbc",
        display_name: "民生银行",
        aliases: &["中国民生银行", "民生", "CMBC"],
    },
    BankDefinition {
        id: "cib",
        display_name: "兴业银行",
        aliases: &["兴业", "CIB"],
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
        aliases: &["平安", "PINGAN"],
    },
    BankDefinition {
        id: "cgb",
        display_name: "广发银行",
        aliases: &["广东发展银行", "广发", "CGB"],
    },
    BankDefinition {
        id: "bob",
        display_name: "北京银行",
        aliases: &["BOB"],
    },
    BankDefinition {
        id: "bos",
        display_name: "上海银行",
        aliases: &["BOS"],
    },
    BankDefinition {
        id: "jsbank",
        display_name: "江苏银行",
        aliases: &["JSB"],
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
        aliases: &["CZB"],
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

pub(crate) fn bank_icon_svg(id: Option<&str>) -> &'static [u8] {
    match id {
        Some("icbc") => include_bytes!("../../assets/banks/icbc.svg"),
        Some("abc") => include_bytes!("../../assets/banks/abc.svg"),
        Some("boc") => include_bytes!("../../assets/banks/boc.svg"),
        Some("ccb") => include_bytes!("../../assets/banks/ccb.svg"),
        Some("bankcomm") => include_bytes!("../../assets/banks/bankcomm.svg"),
        Some("psbc") => include_bytes!("../../assets/banks/psbc.svg"),
        Some("cmb") => include_bytes!("../../assets/banks/cmb.svg"),
        Some("citic") => include_bytes!("../../assets/banks/citic.svg"),
        Some("spdb") => include_bytes!("../../assets/banks/spdb.svg"),
        Some("cmbc") => include_bytes!("../../assets/banks/cmbc.svg"),
        Some("cib") => include_bytes!("../../assets/banks/cib.svg"),
        Some("ceb") => include_bytes!("../../assets/banks/ceb.svg"),
        Some("hxb") => include_bytes!("../../assets/banks/hxb.svg"),
        Some("pingan") => include_bytes!("../../assets/banks/pingan.svg"),
        Some("cgb") => include_bytes!("../../assets/banks/cgb.svg"),
        Some("bob") => include_bytes!("../../assets/banks/bob.svg"),
        Some("bos") => include_bytes!("../../assets/banks/bos.svg"),
        Some("jsbank") => include_bytes!("../../assets/banks/jsbank.svg"),
        Some("njcb") => include_bytes!("../../assets/banks/njcb.svg"),
        Some("nbcb") => include_bytes!("../../assets/banks/nbcb.svg"),
        Some("czbank") => include_bytes!("../../assets/banks/czbank.svg"),
        Some("other") => include_bytes!("../../assets/banks/other.svg"),
        _ => include_bytes!("../../assets/banks/generic.svg"),
    }
}

pub(crate) fn bank_icon(id: Option<&str>) -> slint::Image {
    slint::Image::load_from_svg_data(bank_icon_svg(id)).unwrap_or_default()
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
        assert_eq!(
            legacy_bank_id(Some("中国 工商银行")).as_deref(),
            Some("icbc")
        );
        assert_eq!(legacy_bank_id(Some("icbc")).as_deref(), Some("icbc"));
        assert_eq!(legacy_bank_id(Some("招商")).as_deref(), Some("other"));
        assert_eq!(
            legacy_bank_id(Some("未知地方银行")).as_deref(),
            Some("other")
        );
        assert_eq!(legacy_bank_id(Some("  ")), None);
        assert_eq!(legacy_bank_id(None), None);
    }
}
