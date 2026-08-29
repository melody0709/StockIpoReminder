use std::{
    collections::{HashMap, HashSet},
    path::Path,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, TimeZone, Utc};
use reqwest::blocking::Client;
use serde_json::Value;

use crate::{
    core::{at, china_offset, now_china, parse_date, sha256},
    model::*,
    network::{checked_response, encode_query, ensure_allowed, response_text, text},
};

const METADATA_VERSION: &str = "announcement-metadata-v1";
const ANNOUNCEMENT_PAGE_SIZE: usize = 100;
const MAX_ANNOUNCEMENT_PAGES: usize = 10;

pub(crate) struct ReferenceSearch {
    references: Vec<AnnouncementRef>,
    truncated: bool,
}

impl ReferenceSearch {
    fn output(self, provider: &str) -> SearchOutput {
        SearchOutput {
            references: self.references,
            warning: self.truncated.then(|| {
                format!(
                    "{provider} 公告结果超过 {} 页安全上限，本轮结果已明确标记为不完整",
                    MAX_ANNOUNCEMENT_PAGES
                )
            }),
            used_mirror: false,
        }
    }
}

/// 单页解析结果：保留完整性信息，供分页循环判定是否继续探测。
#[derive(Debug)]
pub(crate) struct ReferencePage {
    references: Vec<AnnouncementRef>,
    /// 原始响应数组行数（过滤前）；total 缺失时以满页探测判断是否还有下一页。
    raw_count: usize,
    /// 来源声明的匹配总数；`None` 表示响应未提供 total，不得回退为行数。
    total: Option<usize>,
}

fn pagination_has_next(page: usize, raw_count: usize, total: Option<usize>) -> Result<bool> {
    if page == 0 {
        bail!("公告分页页码必须从 1 开始");
    }
    match total {
        Some(total) => {
            let consumed_before = (page - 1)
                .checked_mul(ANNOUNCEMENT_PAGE_SIZE)
                .context("公告分页偏移量溢出")?;
            let expected = total
                .saturating_sub(consumed_before)
                .min(ANNOUNCEMENT_PAGE_SIZE);
            if raw_count != expected {
                bail!(
                    "公告分页 total={total} 与第 {page} 页原始行数不一致：实际 {raw_count}，预期 {expected}"
                );
            }
            Ok(consumed_before + raw_count < total)
        }
        None => Ok(raw_count >= ANNOUNCEMENT_PAGE_SIZE),
    }
}

#[derive(Debug)]
pub struct SearchOutput {
    pub references: Vec<AnnouncementRef>,
    pub warning: Option<String>,
    pub used_mirror: bool,
}

impl SearchOutput {
    fn direct(references: Vec<AnnouncementRef>) -> Self {
        Self {
            references,
            warning: None,
            used_mirror: false,
        }
    }
}

mod bse;
mod cninfo;
mod common;
mod search;
mod sse;

#[allow(unused_imports)]
pub(crate) use {bse::*, cninfo::*, common::*, search::*, sse::*};

#[cfg(test)]
mod tests;
