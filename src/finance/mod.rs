mod calendar;
mod credit_card;
mod repo;
mod schedule;

pub(crate) use calendar::*;
pub(crate) use credit_card::*;
pub(crate) use repo::*;
pub(crate) use schedule::*;

use chrono::NaiveDate;

use crate::model::ChinaDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinanceKind {
    ReverseRepo,
    CreditCard,
}

impl FinanceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReverseRepo => "reverse_repo",
            Self::CreditCard => "credit_card",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FinanceReminder {
    pub kind: FinanceKind,
    pub source_id: String,
    pub occurrence_key: String,
    pub event_date: NaiveDate,
    pub due_at: ChinaDateTime,
    pub valid_until: ChinaDateTime,
    pub title: String,
    pub body: String,
    pub primary_action_label: String,
    pub primary_action_cycle: Option<String>,
}

impl FinanceReminder {
    pub(crate) fn open_label(&self) -> &'static str {
        match self.kind {
            FinanceKind::ReverseRepo => "打开逆回购",
            FinanceKind::CreditCard => "打开信用卡",
        }
    }
}
