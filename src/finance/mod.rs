mod bank;
mod calendar;
mod credit_card;
mod repo;
mod schedule;

pub(crate) use bank::*;
pub(crate) use calendar::*;
pub(crate) use credit_card::*;
pub(crate) use repo::*;
pub(crate) use schedule::*;

use chrono::NaiveDate;

use crate::model::ChinaDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum FinanceKind {
    ReverseRepo,
    CreditCard,
    Mortgage,
    AutoLoan,
    CreditLoan,
}

impl FinanceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReverseRepo => "reverse_repo",
            Self::CreditCard => "credit_card",
            Self::Mortgage => "mortgage",
            Self::AutoLoan => "auto_loan",
            Self::CreditLoan => "credit_loan",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FinanceReminderIdentity {
    pub kind: FinanceKind,
    pub source_id: String,
    pub occurrence_key: String,
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
    pub(crate) fn identity(&self) -> FinanceReminderIdentity {
        FinanceReminderIdentity {
            kind: self.kind,
            source_id: self.source_id.clone(),
            occurrence_key: self.occurrence_key.clone(),
        }
    }
}
