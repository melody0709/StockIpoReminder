use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    io::Read,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use reqwest::{
    blocking::{Client, ClientBuilder, Response},
    header::{RANGE, RETRY_AFTER},
    redirect::Policy,
};
use serde_json::Value;

use crate::{core::*, model::*};

const IPO_WINDOW_PAST_DAYS: i64 = 60;
const IPO_WINDOW_FUTURE_DAYS: i64 = 60;
const MAX_BOUNDED_PAGES: usize = 5;
const EASTMONEY_PAGE_SIZE: usize = 100;
const SSE_PAGE_SIZE: usize = 100;
const MAX_DATA_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ANNOUNCEMENT_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 10;
const MAX_RETRY_AFTER_SECONDS: i64 = 24 * 60 * 60;
const EASTMONEY_COLUMNS: &str = "SECURITY_CODE,SECURITY_NAME,MARKET_TYPE_NEW,IS_BEIJING,APPLY_DATE,ISSUE_STATE,APPLY_CODE,ISSUE_PRICE,EACHBALLOT_SHARES,ONLINE_APPLY_UPPER,TOP_APPLY_MARKETCAP,BALLOT_NUM_DATE,BALLOT_PAY_DATE,LISTING_DATE";
const BSE_COLUMNS: &str = "id,fxCode,stockCode,stockName,purchaseDate,issuePrice,issueResultDate,enterPremiumDate,suspendDate,terminationDate";

pub struct CollectorOutput {
    pub source: &'static str,
    pub started: ChinaDateTime,
    pub raw: String,
    pub hash: String,
    pub schema: String,
    pub candidates: Vec<Candidate>,
    pub audit: CollectorAudit,
}

#[derive(Debug, Clone)]
pub struct CollectorAudit {
    pub declared_count: Option<usize>,
    pub detail_count: usize,
    pub accepted_count: usize,
    pub issues: Vec<String>,
}

impl CollectorAudit {
    pub fn state(&self) -> HealthState {
        if self.issues.is_empty() {
            HealthState::Healthy
        } else {
            HealthState::Warning
        }
    }

    pub fn summary(&self) -> Option<String> {
        (!self.issues.is_empty()).then(|| {
            format!(
                "采集计数/明细核验异常：declared={:?}, details={}, accepted={}；{}",
                self.declared_count,
                self.detail_count,
                self.accepted_count,
                self.issues.join("；")
            )
        })
    }
}

#[derive(Debug)]
pub struct HttpStatusError {
    status: u16,
    host: String,
    retry_after: Option<ChinaDateTime>,
}

impl fmt::Display for HttpStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "HTTP {}：{}", self.status, self.host)?;
        if let Some(retry_after) = self.retry_after {
            write!(
                formatter,
                "；Retry-After={}",
                retry_after.format("%Y-%m-%d %H:%M:%S %:z")
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for HttpStatusError {}

mod bse;
mod client;
mod cninfo;
mod common;
mod eastmoney;
mod sse;

#[allow(unused_imports)]
pub(crate) use {bse::*, client::*, cninfo::*, common::*, eastmoney::*, sse::*};

#[cfg(test)]
mod tests;
