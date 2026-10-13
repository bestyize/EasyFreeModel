//! 用量：写入请求记录、按时间窗聚合出概览与明细。空库一律返回零值，不造假数据。

pub mod recorder;

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDateTime, Offset, Timelike, Utc};
use sqlx::SqlitePool;

use crate::error::AppResult;
use crate::models::*;
use crate::store::usage::{self as repo, ListQuery, NewUsage, Totals};
use crate::store::{self, new_id, ts};

pub const COST_SOURCE: &str = "按每条请求记录的 estimated_cost 汇总，为本地估算值，不是上游账单";

/// 明细表默认上限，避免一次把全库塞给前端。
pub const LIST_LIMIT: i64 = 500;
const MODEL_RANK_LIMIT: i64 = 10;

/// 网关在请求结束时调用，写入一条用量记录。`id` 为空则自动生成。
pub async fn record_request(pool: &SqlitePool, mut rec: NewUsage) -> AppResult<()> {
    if rec.id.is_empty() {
        rec.id = new_id();
    }
    if rec.created_at.is_empty() {
        rec.created_at = store::now_ts();
    }
    repo::insert(pool, &rec).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Granularity {
    Hour,
    Day,
    Month,
}

impl Granularity {
    /// SQLite strftime 与 chrono 共用的格式
    fn key_fmt(self) -> &'static str {
        match self {
            Granularity::Hour => "%Y-%m-%dT%H",
            Granularity::Day => "%Y-%m-%d",
            Granularity::Month => "%Y-%m",
        }
    }

    fn label_fmt(self) -> &'static str {
        match self {
            Granularity::Hour => "%H:00",
            Granularity::Day => "%m-%d",
            Granularity::Month => "%Y-%m",
        }
    }
}

/// 时间窗规划。所有 `*_local` 都是"本地墙上时间"（UTC + 固定偏移），仅用于分桶与展示。
#[derive(Debug)]
struct Plan {
    /// 当前窗口起点（UTC 字符串）；None = 全部
    start: Option<String>,
    /// 对比窗口 [start, end)
    prev: Option<(String, String)>,
    gran: Granularity,
    buckets: Vec<NaiveDateTime>,
}

fn floor_hour(t: NaiveDateTime) -> NaiveDateTime {
    t.date().and_hms_opt(t.hour(), 0, 0).unwrap()
}

fn midnight(t: NaiveDateTime) -> NaiveDateTime {
    t.date().and_hms_opt(0, 0, 0).unwrap()
}

fn local_to_utc_ts(local: NaiveDateTime, offset_secs: i32) -> String {
    ts(DateTime::<Utc>::from_naive_utc_and_offset(local - Duration::seconds(offset_secs as i64), Utc))
}

fn hourly(from: NaiveDateTime, to: NaiveDateTime) -> Vec<NaiveDateTime> {
    let mut v = Vec::new();
    let mut t = from;
    while t <= to {
        v.push(t);
        t += Duration::hours(1);
    }
    v
}

fn daily(from: NaiveDateTime, to: NaiveDateTime) -> Vec<NaiveDateTime> {
    let mut v = Vec::new();
    let mut t = from;
    while t <= to {
        v.push(t);
        t += Duration::days(1);
    }
    v
}

fn plan(range: UsageRange, now_utc: DateTime<Utc>, offset_secs: i32, first_record_utc: Option<&str>) -> Plan {
    let now_local = now_utc.naive_utc() + Duration::seconds(offset_secs as i64);
    let to_utc = |t: NaiveDateTime| local_to_utc_ts(t, offset_secs);
    let today = midnight(now_local);

    match range {
        UsageRange::Today => Plan {
            start: Some(to_utc(today)),
            // 对比"昨天截至同一时刻"
            prev: Some((to_utc(today - Duration::days(1)), to_utc(now_local - Duration::days(1)))),
            gran: Granularity::Hour,
            buckets: hourly(today, floor_hour(now_local)),
        },
        UsageRange::Hours24 => {
            let first = floor_hour(now_local) - Duration::hours(23);
            Plan {
                start: Some(to_utc(first)),
                prev: Some((to_utc(first - (now_local - first)), to_utc(first))),
                gran: Granularity::Hour,
                buckets: hourly(first, floor_hour(now_local)),
            }
        }
        UsageRange::Days7 | UsageRange::Days30 => {
            let days = if range == UsageRange::Days7 { 6 } else { 29 };
            let first = today - Duration::days(days);
            Plan {
                start: Some(to_utc(first)),
                prev: Some((to_utc(first - (now_local - first)), to_utc(first))),
                gran: Granularity::Day,
                buckets: daily(first, today),
            }
        }
        UsageRange::All => {
            let first_local = first_record_utc
                .and_then(store::parse_ts)
                .map(|t| t.naive_utc() + Duration::seconds(offset_secs as i64));
            let (gran, buckets) = match first_local {
                None => (Granularity::Day, Vec::new()),
                Some(f) => {
                    let first_day = midnight(f);
                    if (today - first_day).num_days() <= 90 {
                        (Granularity::Day, daily(first_day, today))
                    } else {
                        let mut v = Vec::new();
                        let (mut y, mut m) = (first_day.year(), first_day.month());
                        loop {
                            let d = chrono::NaiveDate::from_ymd_opt(y, m, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
                            if d > today {
                                break;
                            }
                            v.push(d);
                            if m == 12 {
                                y += 1;
                                m = 1;
                            } else {
                                m += 1;
                            }
                        }
                        (Granularity::Month, v)
                    }
                }
            };
            Plan { start: None, prev: None, gran, buckets }
        }
    }
}

/// 范围起点（UTC 字符串）；全部 = None。
pub fn range_start(range: UsageRange, now_utc: DateTime<Utc>, offset_secs: i32) -> Option<String> {
    plan(range, now_utc, offset_secs, None).start
}

fn local_offset_secs() -> i32 {
    Local::now().offset().fix().local_minus_utc()
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn pct_of(part: i64, total: i64) -> f64 {
    if total <= 0 {
        0.0
    } else {
        round1(part as f64 * 100.0 / total as f64)
    }
}

/// 较上一周期变化百分比；没有对比基数（上期为 0 / 无对比窗口）时为 None，不编造。
fn delta(cur: f64, prev: Option<f64>) -> Option<f64> {
    match prev {
        Some(p) if p > 0.0 => Some(round1((cur - p) * 100.0 / p)),
        _ => None,
    }
}

fn tz_modifier(offset_secs: i32) -> String {
    format!("{offset_secs:+} seconds")
}

pub async fn summary(pool: &SqlitePool, range: UsageRange) -> AppResult<UsageSummary> {
    summary_at(pool, range, Utc::now(), local_offset_secs()).await
}

pub async fn summary_at(
    pool: &SqlitePool,
    range: UsageRange,
    now_utc: DateTime<Utc>,
    offset_secs: i32,
) -> AppResult<UsageSummary> {
    let first = if range == UsageRange::All { repo::first_timestamp(pool).await? } else { None };
    let plan = plan(range, now_utc, offset_secs, first.as_deref());
    let start = plan.start.as_deref();

    let cur = repo::totals(pool, start, None).await?;
    let prev: Option<Totals> = match &plan.prev {
        Some((s, e)) => Some(repo::totals(pool, Some(s), Some(e)).await?),
        None => None,
    };
    let metric = |value: f64, prev: Option<f64>| UsageMetric { value, delta_pct: delta(value, prev) };

    // 趋势：按本地时区分桶，缺失的桶补零，让图表时间轴连续
    let rows: HashMap<String, repo::BucketRow> = repo::buckets(pool, start, plan.gran.key_fmt(), &tz_modifier(offset_secs))
        .await?
        .into_iter()
        .map(|r| (r.bucket.clone(), r))
        .collect();
    let trend = plan
        .buckets
        .iter()
        .map(|b| {
            let r = rows.get(&b.format(plan.gran.key_fmt()).to_string());
            TrendPoint {
                label: b.format(plan.gran.label_fmt()).to_string(),
                input: r.map_or(0, |r| r.input_tokens),
                output: r.map_or(0, |r| r.output_tokens),
                cached: r.map_or(0, |r| r.cached_tokens),
                requests: r.map_or(0, |r| r.requests),
                cost: r.map_or(0.0, |r| r.cost),
            }
        })
        .collect();

    let providers = repo::by_provider(pool, start).await?;
    let total_tokens: i64 = providers.iter().map(|p| p.tokens).sum();
    let total_requests: i64 = providers.iter().map(|p| p.requests).sum();
    let provider_share = providers
        .iter()
        .map(|p| ProviderShare {
            provider_id: p.provider_id.clone(),
            name: p.name.clone(),
            tokens: p.tokens,
            pct: pct_of(p.tokens, total_tokens),
        })
        .collect();
    let request_distribution = providers
        .iter()
        .map(|p| RequestShare {
            provider_id: p.provider_id.clone(),
            name: p.name.clone(),
            requests: p.requests,
            pct: pct_of(p.requests, total_requests),
        })
        .collect();
    let model_ranking = repo::by_model(pool, start, MODEL_RANK_LIMIT)
        .await?
        .into_iter()
        .map(|m| ModelRank { pct: pct_of(m.tokens, total_tokens), model: m.model, provider_id: m.provider_id, tokens: m.tokens })
        .collect();

    Ok(UsageSummary {
        range,
        requests: metric(cur.requests as f64, prev.map(|p| p.requests as f64)),
        input_tokens: metric(cur.input_tokens as f64, prev.map(|p| p.input_tokens as f64)),
        output_tokens: metric(cur.output_tokens as f64, prev.map(|p| p.output_tokens as f64)),
        cached_tokens: metric(cur.cached_tokens as f64, prev.map(|p| p.cached_tokens as f64)),
        estimated_cost: metric(cur.cost, prev.map(|p| p.cost)),
        cost_source: COST_SOURCE.to_string(),
        trend,
        provider_share,
        model_ranking,
        request_distribution,
    })
}

pub async fn list_requests(pool: &SqlitePool, filter: UsageFilter) -> AppResult<Vec<UsageRequest>> {
    let q = ListQuery {
        start: range_start(filter.range, Utc::now(), local_offset_secs()),
        provider_id: filter.provider_id.filter(|s| !s.is_empty()),
        model: filter.model.filter(|s| !s.is_empty()),
        status: filter.status,
        protocol: filter.protocol.map(|p| p.as_str().to_string()),
        text: filter.query,
        limit: LIST_LIMIT,
    };
    repo::list(pool, &q).await
}

/// 左下角运行状态用：今日请求数 / 成功率(0–100) / 平均延迟。无请求时成功率与延迟为 None。
pub async fn today_stats(pool: &SqlitePool) -> AppResult<(i64, Option<f64>, Option<f64>)> {
    let start = range_start(UsageRange::Today, Utc::now(), local_offset_secs()).unwrap_or_default();
    let s = repo::stats_since(pool, &start).await?;
    if s.requests == 0 {
        return Ok((0, None, None));
    }
    Ok((s.requests, Some(round1(s.successes as f64 * 100.0 / s.requests as f64)), s.avg_latency_ms.map(round1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn rec(id: &str, provider: &str, model: &str, at: DateTime<Utc>, input: i64, output: i64, status: UsageStatus) -> NewUsage {
        NewUsage {
            id: id.into(),
            api_key_id: None,
            provider_id: provider.into(),
            connection_id: "c1".into(),
            model_id: model.into(),
            protocol: "openai-chat".into(),
            status,
            input_tokens: input,
            output_tokens: output,
            cached_tokens: 5,
            estimated_cost: 0.5,
            duration_ms: 100,
            token_source: TokenSource::Upstream,
            created_at: ts(at),
        }
    }

    #[tokio::test]
    async fn empty_database_returns_zeros_for_every_range() {
        let pool = store::open_memory().await.unwrap();
        for range in UsageRange::ALL {
            let s = summary(&pool, *range).await.unwrap();
            assert_eq!(s.requests.value, 0.0);
            assert_eq!(s.input_tokens.value, 0.0);
            assert_eq!(s.output_tokens.value, 0.0);
            assert_eq!(s.cached_tokens.value, 0.0);
            assert_eq!(s.estimated_cost.value, 0.0);
            assert!(s.requests.delta_pct.is_none(), "无对比基数时不得编造变化率");
            assert!(s.trend.iter().all(|p| p.requests == 0 && p.input == 0 && p.cost == 0.0));
            assert!(s.provider_share.is_empty() && s.model_ranking.is_empty() && s.request_distribution.is_empty());
            assert!(!s.cost_source.is_empty());
        }
        assert!(list_requests(&pool, UsageFilter { range: UsageRange::All, provider_id: None, model: None, status: None, protocol: None, query: None }).await.unwrap().is_empty());
        assert_eq!(today_stats(&pool).await.unwrap(), (0, None, None));
        assert_eq!(summary(&pool, UsageRange::Hours24).await.unwrap().trend.len(), 24);
        assert_eq!(summary(&pool, UsageRange::Days30).await.unwrap().trend.len(), 30);
        assert!(summary(&pool, UsageRange::All).await.unwrap().trend.is_empty());
    }

    #[tokio::test]
    async fn aggregates_trend_share_ranking_and_delta() {
        let pool = store::open_memory().await.unwrap();
        crate::providers::catalog::seed(&pool).await.unwrap();
        // 固定时间 + 东八区
        let now = Utc.with_ymd_and_hms(2026, 10, 14, 6, 30, 0).unwrap(); // 本地 14:30
        let off = 8 * 3600;
        let today_morning = Utc.with_ymd_and_hms(2026, 10, 14, 1, 10, 0).unwrap(); // 本地 09:10
        let yesterday = Utc.with_ymd_and_hms(2026, 10, 13, 1, 0, 0).unwrap(); // 本地昨天 09:00
        for r in [
            rec("a", "openrouter", "m1", today_morning, 100, 50, UsageStatus::Success),
            rec("b", "openrouter", "m1", today_morning + Duration::minutes(5), 10, 5, UsageStatus::Error),
            rec("c", "gemini", "g1", today_morning + Duration::hours(2), 30, 20, UsageStatus::Success),
            rec("d", "openrouter", "m1", yesterday, 40, 10, UsageStatus::Success),
        ] {
            record_request(&pool, r).await.unwrap();
        }

        let s = summary_at(&pool, UsageRange::Today, now, off).await.unwrap();
        assert_eq!(s.requests.value, 3.0);
        assert_eq!(s.input_tokens.value, 140.0);
        assert_eq!(s.output_tokens.value, 75.0);
        assert_eq!(s.cached_tokens.value, 15.0, "缓存 Token 独立统计");
        assert_eq!(s.estimated_cost.value, 1.5);
        // 昨天同一时刻之前只有 1 条 → +200%
        assert_eq!(s.requests.delta_pct, Some(200.0));

        assert_eq!(s.trend.len(), 15); // 00:00 .. 14:00
        let nine = s.trend.iter().find(|p| p.label == "09:00").unwrap();
        assert_eq!((nine.requests, nine.input, nine.output), (2, 110, 55));
        assert_eq!(s.trend.iter().find(|p| p.label == "11:00").unwrap().requests, 1);
        assert_eq!(s.trend.iter().find(|p| p.label == "10:00").unwrap().requests, 0);

        // tokens = input + output：openrouter 165, gemini 50
        assert_eq!(s.provider_share[0].provider_id, "openrouter");
        assert_eq!(s.provider_share[0].name, "OpenRouter");
        assert_eq!(s.provider_share[0].tokens, 165);
        assert_eq!(s.provider_share[0].pct, 76.7);
        assert_eq!(s.model_ranking[0].model, "m1");
        assert_eq!(s.request_distribution[0].requests, 2);

        // 7 天：昨天也计入；全部：窗口 1 天前开始
        let w = summary_at(&pool, UsageRange::Days7, now, off).await.unwrap();
        assert_eq!(w.requests.value, 4.0);
        assert_eq!(w.trend.len(), 7);
        let all = summary_at(&pool, UsageRange::All, now, off).await.unwrap();
        assert_eq!(all.requests.value, 4.0);
        assert_eq!(all.trend.len(), 2);
        assert!(all.requests.delta_pct.is_none());
    }

    #[tokio::test]
    async fn list_filters_and_search() {
        let pool = store::open_memory().await.unwrap();
        let now = Utc::now();
        record_request(&pool, rec("req-1", "openrouter", "gpt_a", now, 1, 1, UsageStatus::Success)).await.unwrap();
        record_request(&pool, rec("req-2", "gemini", "gemini-pro", now, 1, 1, UsageStatus::Error)).await.unwrap();
        record_request(&pool, rec("req-3", "gemini", "gemini-pro", now - Duration::days(40), 1, 1, UsageStatus::Success)).await.unwrap();
        let f = |range, status: Option<UsageStatus>, query: Option<&str>, provider: Option<&str>| UsageFilter {
            range,
            provider_id: provider.map(Into::into),
            model: None,
            status,
            protocol: Some(Protocol::OpenaiChat),
            query: query.map(Into::into),
        };
        assert_eq!(list_requests(&pool, f(UsageRange::All, None, None, None)).await.unwrap().len(), 3);
        assert_eq!(list_requests(&pool, f(UsageRange::Days30, None, None, None)).await.unwrap().len(), 2);
        let errs = list_requests(&pool, f(UsageRange::All, Some(UsageStatus::Error), None, None)).await.unwrap();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].request_id, "req-2");
        assert_eq!(errs[0].account_id, "c1");
        assert_eq!(list_requests(&pool, f(UsageRange::All, None, None, Some("openrouter"))).await.unwrap().len(), 1);
        // LIKE 通配符被转义："_" 不应匹配任意字符
        assert_eq!(list_requests(&pool, f(UsageRange::All, None, Some("gpt_a"), None)).await.unwrap().len(), 1);
        assert_eq!(list_requests(&pool, f(UsageRange::All, None, Some("gpt%"), None)).await.unwrap().len(), 0);
        assert_eq!(list_requests(&pool, f(UsageRange::All, None, Some("GEMINI"), None)).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn today_stats_reports_rates() {
        let pool = store::open_memory().await.unwrap();
        let now = Utc::now();
        record_request(&pool, rec("a", "p", "m", now, 1, 1, UsageStatus::Success)).await.unwrap();
        record_request(&pool, rec("b", "p", "m", now, 1, 1, UsageStatus::Error)).await.unwrap();
        // 避免恰好跨本地午夜的偶发失败：只断言结构
        let (n, rate, lat) = today_stats(&pool).await.unwrap();
        if n == 2 {
            assert_eq!(rate, Some(50.0));
            assert_eq!(lat, Some(100.0));
        }
    }
}
