use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

pub type ChinaDateTime = DateTime<FixedOffset>;

macro_rules! numeric_enum {
    ($name:ident { $($variant:ident = $value:expr),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize_repr, Deserialize_repr)]
        #[repr(i32)]
        pub enum $name { $($variant = $value),+ }
        impl $name {
            #[allow(dead_code)]
            pub fn from_i32(value: i32) -> Self {
                match value { $($value => Self::$variant,)+ _ => Self::Unknown }
            }
        }
    };
}

numeric_enum!(Exchange { Unknown = 0, Shanghai = 1, Shenzhen = 2, Beijing = 3 });
numeric_enum!(Board { Unknown = 0, Main = 1, Star = 2, ChiNext = 3, Beijing = 4 });
numeric_enum!(IssueStatus { Unknown = 0, Upcoming = 1, Active = 2, Postponed = 3, Suspended = 4, Terminated = 5, Completed = 6 });
numeric_enum!(LifecycleStatus { Unknown = -1, Discovered = 0, Scheduled = 1, ActiveUnconfirmed = 2, Acknowledged = 3, AcknowledgedNeedsReview = 4, SuspendedOrCancelled = 5, Superseded = 6, ExpiredUnconfirmed = 7 });
numeric_enum!(DataQualityStatus { Unknown = -1, SingleSource = 0, MultiSourceVerified = 1, AnnouncementVerified = 2, DataConflict = 3, Stale = 4, ManualReviewRequired = 5 });
numeric_enum!(FundingMode { Unknown = -1, MarketValue = 0, FullCash = 1 });
numeric_enum!(ReminderLevel { Unknown = -1, Advance = 0, Morning = 10, BrokerOpening = 15, MarketOpening = 20, Hourly = 30, NoonBoundary = 40, AfternoonOpening = 45, FifteenMinutes = 50, FiveMinutes = 60, TwoMinutes = 70, Final = 80, DataChanged = 90, HealthWarning = 100, BallotCheck = 110, PaymentMorning = 120, PaymentFollowUp = 130, ListingMorning = 140 });
numeric_enum!(DeliveryState { Unknown = -1, Pending = 0, Leased = 1, Delivered = 2, Collapsed = 3, Cancelled = 4, Failed = 5 });
numeric_enum!(SecondaryNotificationProvider { Unknown = -1, Disabled = 0, WeCom = 1, DingTalk = 2, Feishu = 3, PushPlus = 4 });
numeric_enum!(HealthState { Unknown = 0, Healthy = 1, Warning = 2, Failed = 3 });
numeric_enum!(ExtractionStatus { Unknown = -1, Pending = 0, Extracted = 1, LowConfidence = 2, Failed = 3, Unsupported = 4 });
numeric_enum!(SyncConclusionKind { Unknown = 0, HealthyNonempty = 1, HealthyEmpty = 2, DegradedCached = 3 });

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionSession {
    pub session_number: i32,
    #[serde(with = "time_format")]
    pub official_start: NaiveTime,
    #[serde(with = "time_format")]
    pub official_end: NaiveTime,
    #[serde(default, with = "optional_time_format")]
    pub broker_accept_start: Option<NaiveTime>,
    #[serde(default, with = "optional_time_format")]
    pub safety_cutoff: Option<NaiveTime>,
    pub funding_mode: FundingMode,
    pub allocation_time_sensitive: bool,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub source_published_at: Option<ChinaDateTime>,
}

fn default_source() -> String {
    "default".to_owned()
}

#[derive(Debug, Clone)]
pub struct IpoEvent {
    pub id: String,
    pub exchange: Exchange,
    pub board: Board,
    pub security_code: String,
    pub apply_code: Option<String>,
    pub legacy_code: Option<String>,
    pub name: String,
    pub apply_date: Option<NaiveDate>,
    pub issue_price: Option<f64>,
    pub lot_size: Option<i64>,
    pub max_apply_quantity: Option<i64>,
    pub required_market_value: Option<f64>,
    pub required_cash: Option<f64>,
    pub ballot_date: Option<NaiveDate>,
    pub payment_date: Option<NaiveDate>,
    pub listing_date: Option<NaiveDate>,
    pub status: IssueStatus,
    pub lifecycle_status: LifecycleStatus,
    pub event_version: i32,
    pub announcement_url: Option<String>,
    pub data_quality_status: DataQualityStatus,
    pub data_conflict: bool,
    pub manual_override_fields: Vec<String>,
    pub sessions: Vec<SubscriptionSession>,
    pub first_seen_at: ChinaDateTime,
    pub updated_at: ChinaDateTime,
}

impl IpoEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            IssueStatus::Terminated | IssueStatus::Suspended
        ) || matches!(
            self.lifecycle_status,
            LifecycleStatus::SuspendedOrCancelled
                | LifecycleStatus::ExpiredUnconfirmed
                | LifecycleStatus::Superseded
        )
    }
    pub fn display_code(&self) -> &str {
        self.apply_code.as_deref().unwrap_or(&self.security_code)
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub source: String,
    pub priority: i32,
    pub fetched_at: ChinaDateTime,
    pub published_at: Option<ChinaDateTime>,
    pub exchange: Exchange,
    pub board: Board,
    pub security_code: Option<String>,
    pub apply_code: Option<String>,
    pub legacy_code: Option<String>,
    pub name: Option<String>,
    pub apply_date: Option<NaiveDate>,
    pub issue_price: Option<f64>,
    pub lot_size: Option<i64>,
    pub max_apply_quantity: Option<i64>,
    pub required_market_value: Option<f64>,
    pub required_cash: Option<f64>,
    pub ballot_date: Option<NaiveDate>,
    pub payment_date: Option<NaiveDate>,
    pub listing_date: Option<NaiveDate>,
    pub status: IssueStatus,
    pub announcement_url: Option<String>,
    pub sessions: Vec<SubscriptionSession>,
}

impl Candidate {
    pub fn stable_identity(&self) -> Option<String> {
        self.security_code
            .as_ref()
            .map(|code| format!("{}:{code}", exchange_name(self.exchange)))
            .or_else(|| {
                self.apply_code
                    .as_ref()
                    .map(|code| format!("{}:apply:{code}", exchange_name(self.exchange)))
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub shanghai_enabled: bool,
    pub shenzhen_enabled: bool,
    pub beijing_enabled: bool,
    #[serde(with = "time_format")]
    pub shanghai_broker_accept_start: NaiveTime,
    #[serde(with = "time_format")]
    pub shenzhen_broker_accept_start: NaiveTime,
    #[serde(with = "time_format")]
    pub beijing_broker_accept_start: NaiveTime,
    #[serde(with = "time_format")]
    pub safety_cutoff: NaiveTime,
    pub beijing_reservation_supported: bool,
    pub sound_enabled: bool,
    pub flash_taskbar: bool,
    pub toast_enabled: bool,
    pub daily_health_summary_enabled: bool,
    pub post_apply_reminders_enabled: bool,
    pub listing_reminders_enabled: bool,
    pub automatic_updates_enabled: bool,
    pub crash_report_upload_enabled: bool,
    pub secondary_notification_enabled: bool,
    pub secondary_notification_provider: SecondaryNotificationProvider,
    pub auto_start_enabled: bool,
    pub normal_sync_minutes: i32,
    pub active_day_sync_minutes: i32,
    pub notification_self_test_completed: bool,
    pub notification_window_test_passed: Option<bool>,
    pub notification_toast_test_passed: Option<bool>,
    pub notification_balloon_test_passed: Option<bool>,
    pub notification_sound_test_passed: Option<bool>,
    pub notification_flash_test_passed: Option<bool>,
    pub onboarding_completed: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            shanghai_enabled: true,
            shenzhen_enabled: true,
            beijing_enabled: true,
            shanghai_broker_accept_start: time(9, 30),
            shenzhen_broker_accept_start: time(9, 15),
            beijing_broker_accept_start: time(9, 15),
            safety_cutoff: time(14, 55),
            beijing_reservation_supported: false,
            sound_enabled: true,
            flash_taskbar: true,
            toast_enabled: true,
            daily_health_summary_enabled: true,
            post_apply_reminders_enabled: true,
            listing_reminders_enabled: true,
            automatic_updates_enabled: false,
            crash_report_upload_enabled: false,
            secondary_notification_enabled: false,
            secondary_notification_provider: SecondaryNotificationProvider::Disabled,
            auto_start_enabled: true,
            normal_sync_minutes: 30,
            active_day_sync_minutes: 10,
            notification_self_test_completed: false,
            notification_window_test_passed: None,
            notification_toast_test_passed: None,
            notification_balloon_test_passed: None,
            notification_sound_test_passed: None,
            notification_flash_test_passed: None,
            onboarding_completed: false,
        }
    }
}

impl AppSettings {
    pub fn exchange_enabled(&self, exchange: Exchange) -> bool {
        match exchange {
            Exchange::Shanghai => self.shanghai_enabled,
            Exchange::Shenzhen => self.shenzhen_enabled,
            Exchange::Beijing => self.beijing_enabled,
            _ => false,
        }
    }

    pub fn notification_tests_complete(&self) -> bool {
        self.notification_window_test_passed == Some(true)
            && (!self.toast_enabled
                || self.notification_toast_test_passed == Some(true)
                || self.notification_balloon_test_passed == Some(true))
            && (!self.sound_enabled || self.notification_sound_test_passed == Some(true))
            && (!self.flash_taskbar || self.notification_flash_test_passed == Some(true))
    }

    pub fn notification_tests_started(&self) -> bool {
        self.notification_window_test_passed.is_some()
            || self.notification_toast_test_passed.is_some()
            || self.notification_balloon_test_passed.is_some()
            || self.notification_sound_test_passed.is_some()
            || self.notification_flash_test_passed.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct ReminderItem {
    pub event_id: String,
    pub event_version: i32,
    pub due_at: ChinaDateTime,
    pub level: ReminderLevel,
    pub dedupe_key: String,
}

#[derive(Debug, Clone)]
pub struct ReminderDelivery {
    pub outbox_id: i64,
    pub event: IpoEvent,
    pub due_at: ChinaDateTime,
    pub level: ReminderLevel,
    pub dedupe_key: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub attempt_count: i32,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SecondaryNotificationDelivery {
    pub id: i64,
    pub reminder_outbox_id: i64,
    pub request_attempt_id: i64,
    pub provider: SecondaryNotificationProvider,
    pub event: IpoEvent,
    pub due_at: ChinaDateTime,
    pub level: ReminderLevel,
    pub attempt_count: i32,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecondaryNotificationSummary {
    pub pending: i64,
    pub leased: i64,
    pub delivered: i64,
    pub retrying: i64,
    pub exhausted: i64,
    pub cancelled: i64,
    pub requests_last_hour: i64,
    pub latest_success_at: Option<ChinaDateTime>,
    pub latest_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedField {
    pub name: String,
    pub value: String,
    pub confidence: f64,
    pub evidence: Option<String>,
    pub character_offset: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct AnnouncementRef {
    pub provider: String,
    pub announcement_id: String,
    pub title: String,
    pub url: String,
    pub published_at: Option<ChinaDateTime>,
    pub announcement_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnnouncementDocument {
    pub id: String,
    pub event_id: String,
    pub reference: AnnouncementRef,
    pub local_path: String,
    pub file_hash: String,
    pub text_hash: Option<String>,
    pub status: ExtractionStatus,
    pub parser_version: String,
    pub fields: Vec<ParsedField>,
    pub downloaded_at: ChinaDateTime,
}

#[derive(Debug, Clone)]
pub struct FieldSourceEntry {
    pub field_name: String,
    pub raw_value: Option<String>,
    pub normalized_value: Option<String>,
    pub source: String,
    pub priority: i32,
    pub fetched_at: ChinaDateTime,
}

#[derive(Debug, Clone)]
pub struct ManualOverrideEntry {
    pub id: i64,
    pub field_name: String,
    pub override_value: String,
    pub reason: String,
    pub announcement_document_id: Option<String>,
    pub created_at: ChinaDateTime,
    pub revoked_at: Option<ChinaDateTime>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceHealthEntry {
    pub source: String,
    pub state: HealthState,
    pub last_record_count: i64,
    pub last_success_at: Option<ChinaDateTime>,
    pub consecutive_failures: i32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationHealthEntry {
    pub component: String,
    pub state: HealthState,
    pub last_attempt_at: Option<ChinaDateTime>,
    pub last_success_at: Option<ChinaDateTime>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthDetails {
    pub overall_state: HealthState,
    pub today_task_count: usize,
    pub pending_confirmation_count: usize,
    pub conflict_count: usize,
    pub manual_review_count: usize,
    pub delivery_retry_count: usize,
    pub oldest_delivery_retry_at: Option<ChinaDateTime>,
    pub latest_delivery_error: Option<String>,
    pub scheduler_heartbeat: Option<ChinaDateTime>,
    pub delivery_heartbeat: Option<ChinaDateTime>,
    pub sources: Vec<SourceHealthEntry>,
    pub operations: Vec<OperationHealthEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRunSummary {
    pub source: String,
    pub started_at: ChinaDateTime,
    pub finished_at: ChinaDateTime,
    pub success: bool,
    pub record_count: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderLogSummary {
    pub event_id: String,
    pub scheduled_at: ChinaDateTime,
    pub shown_at: ChinaDateTime,
    pub reminder_level: ReminderLevel,
    pub delivery_channel: String,
    pub result: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderStateSummary {
    pub pending: i64,
    pub leased: i64,
    pub delivered: i64,
    pub collapsed: i64,
    pub cancelled: i64,
    pub failed: i64,
    pub oldest_failed_at: Option<ChinaDateTime>,
    pub latest_error: Option<String>,
    pub shown_last_seven_days: i64,
    pub latest_shown_at: Option<ChinaDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConclusion {
    pub kind: SyncConclusionKind,
    pub started_at: ChinaDateTime,
    pub finished_at: ChinaDateTime,
    pub today_count: usize,
    pub event_count: usize,
    pub announcement_count: usize,
    pub successful_sources: Vec<String>,
    pub missing_sources: Vec<String>,
    pub summary: String,
}

impl SyncConclusionKind {
    pub fn is_healthy(self) -> bool {
        matches!(self, Self::HealthyNonempty | Self::HealthyEmpty)
    }

    pub fn health_state(self) -> HealthState {
        match self {
            Self::HealthyNonempty | Self::HealthyEmpty => HealthState::Healthy,
            Self::DegradedCached => HealthState::Warning,
            Self::Unknown => HealthState::Failed,
        }
    }
}

pub fn exchange_name(exchange: Exchange) -> &'static str {
    match exchange {
        Exchange::Shanghai => "shanghai",
        Exchange::Shenzhen => "shenzhen",
        Exchange::Beijing => "beijing",
        _ => "unknown",
    }
}
pub fn time(hour: u32, minute: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(hour, minute, 0).expect("valid fixed time")
}

mod time_format {
    use chrono::NaiveTime;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &NaiveTime, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.format("%H:%M:%S").to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<NaiveTime, D::Error> {
        let value = String::deserialize(deserializer)?;
        NaiveTime::parse_from_str(&value, "%H:%M:%S")
            .or_else(|_| NaiveTime::parse_from_str(&value, "%H:%M"))
            .map_err(serde::de::Error::custom)
    }
}
mod optional_time_format {
    use chrono::NaiveTime;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        value: &Option<NaiveTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => serializer.serialize_some(&value.format("%H:%M:%S").to_string()),
            None => serializer.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<NaiveTime>, D::Error> {
        let value = Option::<String>::deserialize(deserializer)?;
        value
            .map(|v| {
                NaiveTime::parse_from_str(&v, "%H:%M:%S")
                    .or_else(|_| NaiveTime::parse_from_str(&v, "%H:%M"))
                    .map_err(serde::de::Error::custom)
            })
            .transpose()
    }
}
