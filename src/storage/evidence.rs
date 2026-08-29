use super::*;

impl Database {
    pub fn save_announcement(&self, document: &AnnouncementDocument) -> Result<()> {
        self.open()?.execute("INSERT OR IGNORE INTO announcement_documents(id,ipo_event_id,provider,announcement_id,announcement_type,title,published_at,source_url,local_path,file_hash,extraction_status,extracted_text_hash,parser_version,parsed_fields_json,downloaded_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",params![document.id,document.event_id,document.reference.provider,document.reference.announcement_id,document.reference.announcement_type,document.reference.title,document.reference.published_at.map(format_dt),document.reference.url,document.local_path,document.file_hash,document.status as i32,document.text_hash,document.parser_version,serde_json::to_string(&document.fields)?,format_dt(document.downloaded_at)])?;
        Ok(())
    }
    pub fn replace_field_sources(&self, event_id: &str, candidates: &[Candidate]) -> Result<()> {
        type StableFieldSource = (
            String,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            i32,
        );
        let mut desired = Vec::<(StableFieldSource, String)>::new();
        for candidate in candidates {
            let values = [
                ("SecurityCode", candidate.security_code.clone()),
                ("ApplyCode", candidate.apply_code.clone()),
                ("LegacyCode", candidate.legacy_code.clone()),
                ("Name", candidate.name.clone()),
                ("ApplyDate", candidate.apply_date.map(format_date)),
                (
                    "IssuePrice",
                    candidate.issue_price.map(|value| value.to_string()),
                ),
                ("LotSize", candidate.lot_size.map(|value| value.to_string())),
                (
                    "MaxApplyQuantity",
                    candidate.max_apply_quantity.map(|value| value.to_string()),
                ),
                (
                    "RequiredMarketValue",
                    candidate
                        .required_market_value
                        .map(|value| value.to_string()),
                ),
                (
                    "RequiredCash",
                    candidate.required_cash.map(|value| value.to_string()),
                ),
                ("BallotDate", candidate.ballot_date.map(format_date)),
                ("PaymentDate", candidate.payment_date.map(format_date)),
                ("ListingDate", candidate.listing_date.map(format_date)),
                ("IssueStatus", Some((candidate.status as i32).to_string())),
            ];
            for (field, value) in values {
                let Some(value) = value else { continue };
                desired.push((
                    (
                        field.to_owned(),
                        Some(value.clone()),
                        Some(value),
                        candidate.source.clone(),
                        candidate.published_at.map(format_dt),
                        None,
                        candidate.priority,
                    ),
                    format_dt(candidate.fetched_at),
                ));
            }
        }

        let mut connection = self.open()?;
        let mut current = {
            let mut statement = connection.prepare(
                "SELECT field_name,normalized_value,raw_value,source,source_published_at,raw_hash,priority
                 FROM ipo_field_sources WHERE ipo_event_id=?1",
            )?;
            statement
                .query_map([event_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<StableFieldSource>>>()?
        };
        let mut desired_stable = desired
            .iter()
            .map(|(stable, _)| stable.clone())
            .collect::<Vec<_>>();
        current.sort();
        desired_stable.sort();
        if current == desired_stable {
            return Ok(());
        }

        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM ipo_field_sources WHERE ipo_event_id=?1",
            [event_id],
        )?;
        for ((field, normalized, raw, source, published_at, raw_hash, priority), fetched_at) in
            desired
        {
            transaction.execute(
                "INSERT INTO ipo_field_sources(ipo_event_id,field_name,normalized_value,raw_value,source,source_published_at,fetched_at,raw_hash,priority) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    event_id,
                    field,
                    normalized,
                    raw,
                    source,
                    published_at,
                    fetched_at,
                    raw_hash,
                    priority,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub fn announcement_titles(&self, event_id: &str) -> Result<Vec<String>> {
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT title FROM announcement_documents WHERE ipo_event_id=?1 ORDER BY published_at DESC,downloaded_at DESC")?;
        let rows = statement.query_map([event_id], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn field_sources(&self, event_id: &str) -> Result<Vec<FieldSourceEntry>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT field_name,raw_value,normalized_value,source,priority,fetched_at FROM ipo_field_sources WHERE ipo_event_id=?1 ORDER BY field_name,priority DESC,fetched_at DESC,id",
        )?;
        let rows = statement.query_map([event_id], |row| {
            let fetched_at: String = row.get(5)?;
            Ok(FieldSourceEntry {
                field_name: row.get(0)?,
                raw_value: row.get(1)?,
                normalized_value: row.get(2)?,
                source: row.get(3)?,
                priority: row.get(4)?,
                fetched_at: parse_dt(&fetched_at).map_err(to_sql_error)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn announcements(&self, event_id: &str) -> Result<Vec<AnnouncementDocument>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id,ipo_event_id,provider,announcement_id,announcement_type,title,published_at,source_url,local_path,file_hash,extraction_status,extracted_text_hash,parser_version,parsed_fields_json,downloaded_at FROM announcement_documents WHERE ipo_event_id=?1 ORDER BY published_at DESC,downloaded_at DESC,id",
        )?;
        let rows = statement.query_map([event_id], |row| {
            let published_at: Option<String> = row.get(6)?;
            let fields_json: String = row.get(13)?;
            let downloaded_at: String = row.get(14)?;
            Ok(AnnouncementDocument {
                id: row.get(0)?,
                event_id: row.get(1)?,
                reference: AnnouncementRef {
                    provider: row.get(2)?,
                    announcement_id: row.get(3)?,
                    announcement_type: row.get(4)?,
                    title: row.get(5)?,
                    published_at: published_at
                        .as_deref()
                        .and_then(|value| parse_dt(value).ok()),
                    url: row.get(7)?,
                },
                local_path: row.get(8)?,
                file_hash: row.get(9)?,
                status: ExtractionStatus::from_i32_tracked("extraction_status", row.get(10)?),
                text_hash: row.get(11)?,
                parser_version: row.get(12)?,
                fields: serde_json::from_str(&fields_json)
                    .map_err(|error| to_sql_error(error.into()))?,
                downloaded_at: parse_dt(&downloaded_at).map_err(to_sql_error)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
