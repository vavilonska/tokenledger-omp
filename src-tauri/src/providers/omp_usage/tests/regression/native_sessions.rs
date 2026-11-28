use std::{io::Write, path::Path};

use chrono::Duration;

use super::*;
use crate::models::UsagePeriod;

const ACCOUNT: &str = "account-a";
const STARTED: &str = "2026-07-11T13:00:00Z";
const FIRST: &str = "2026-07-12T14:00:00Z";
const SECOND: &str = "2026-07-12T14:01:00Z";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 12, 15, 0, 0).unwrap()
}

fn paid(id: &str, timestamp: &str, input: u64) -> Value {
    json!({"type":"message", "id":id, "timestamp":timestamp,
        "message":{"role":"assistant", "provider":"openai-codex", "model":"gpt-6.2-sol",
            "serviceTier":"default", "usage":{"input":input,"output":0,"cacheRead":0,"cacheWrite":0}}})
}

fn write_journal(path: &Path, id: &str, started: &str, parent: Option<&str>, entries: &[Value]) {
    let folder = path.parent().unwrap();
    fs::create_dir_all(folder).unwrap();
    let mut header = json!({"type":"session", "id":id, "version":3, "timestamp":started,
        "cwd":folder.to_str().unwrap()});
    if let Some(parent) = parent {
        header["parentSession"] = json!(parent);
    }
    let mut journal = fs::File::create(path).unwrap();
    writeln!(journal, "{header}").unwrap();
    for entry in entries {
        writeln!(journal, "{entry}").unwrap();
    }
}

fn append(path: &Path, entry: Value) {
    let mut journal = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(journal, "{entry}").unwrap();
}

fn write_marker(home: &Path, name: &str, contents: &str) {
    let file = home.join("agent/custom-session-files").join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, contents).unwrap();
}

fn register(home: &Path, name: &str, target: &Path) {
    assert!(target.is_absolute());
    write_marker(home, name, target.to_str().unwrap());
}

fn scan_home(
    storage: &Storage,
    home: &Path,
    account: Option<&str>,
    now: DateTime<Utc>,
    pricing: &ModelPricing,
) -> (OmpScanOutcome, [UsageHistory; 3]) {
    let mut daily = DailyUsageAccumulator::default();
    let mut session = DailyUsageAccumulator::default();
    let mut weekly = DailyUsageAccumulator::default();
    let outcome = scan_home_into(
        storage,
        home,
        now,
        pricing,
        account,
        Some(now - Duration::hours(5)),
        Some(now - Duration::days(7)),
        &mut daily,
        &mut session,
        &mut weekly,
    )
    .unwrap();
    (
        outcome,
        [daily, session, weekly].map(|usage| usage.build(now, "test")),
    )
}

fn assert_cost(actual: Option<f64>, expected: f64) {
    let actual = actual.unwrap();
    assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
}

fn assert_period(period: &UsagePeriod, tokens: u64, cost: f64) {
    assert_eq!(period.tokens, tokens);
    assert!(period.estimate_complete);
    assert!(period.unknown_models.is_empty());
    assert_cost(period.estimated_cost_usd, cost);
}

fn assert_usage(
    outcome: &OmpScanOutcome,
    histories: &[UsageHistory; 3],
    totals: &[u64],
    cost: f64,
) {
    assert!(outcome.included);
    let mut actual = outcome
        .events
        .iter()
        .map(|event| event.total)
        .collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(actual, totals);
    for history in histories {
        assert_period(
            history.last_30_days.as_ref().unwrap(),
            totals.iter().sum(),
            cost,
        );
    }
}

#[test]
fn native_default_and_custom_trees_share_dedup_cache_retention_and_auth_gate() {
    let directory = tempdir().unwrap();
    // Some Unix temp directories live below a system alias (e.g. /var on macOS).
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("omp");
    create_databases(&home, ACCOUNT);
    let default = home.join("agent/sessions/parent.jsonl");
    let default_child = default.with_extension("").join("child.jsonl");
    let custom = root.join("workspace/custom.jsonl");
    let child = custom.with_extension("").join("child.jsonl");
    let fork = child.with_extension("").join("fork.jsonl");

    let mut shared = paid("shared-default", FIRST, 100);
    shared["message"]["responseId"] = json!("resp-cross-root");
    let mut copy = shared.clone();
    copy["id"] = json!("shared-custom");
    write_journal(&default, "default", STARTED, None, &[shared]);
    write_journal(
        &default_child,
        "default-child",
        STARTED,
        Some("default"),
        &[paid("default-child", FIRST, 20)],
    );
    let mut custom_call = paid("custom", FIRST, 200);
    custom_call["message"]["credentialId"] = json!("synthetic-other-credential");
    write_journal(&custom, "custom", STARTED, None, &[copy, custom_call]);
    let original = paid("original-child", FIRST, 300);
    write_journal(
        &child,
        "child",
        STARTED,
        custom.to_str(),
        std::slice::from_ref(&original),
    );
    let mut inherited = original;
    inherited["message"]["serviceTier"] = json!("unrecognized");
    write_journal(
        &fork,
        "fork",
        SECOND,
        Some("child"),
        &[inherited, paid("new-in-fork", SECOND, 400)],
    );
    register(&home, "overlapping-default", &default);
    register(&home, "custom", &custom);

    let database = root.join("cache.db");
    let storage = Storage::open(&database).unwrap();
    let pricing = crate::pricing::test_bundled_pricing();
    let scan = |storage: &Storage, account| scan_home(storage, &home, account, now(), &pricing);
    let assert_skipped = |storage: &Storage, account| {
        let (outcome, histories) = scan(storage, account);
        assert!(!outcome.included);
        assert!(outcome.events.is_empty());
        assert!(outcome.weekly_anchors.is_empty());
        assert!(histories
            .iter()
            .all(|history| *history == UsageHistory::default()));
    };
    let assert_matching = |storage: &Storage, totals: &[u64], cost| {
        let (outcome, histories) = scan(storage, Some(ACCOUNT));
        assert_usage(&outcome, &histories, totals, cost);
        assert_eq!(outcome.weekly_anchors.len(), 1);
        let original = outcome
            .events
            .iter()
            .find(|event| event.total == 300)
            .unwrap();
        assert_cost(original.cost, 0.0006);
    };

    assert_skipped(&storage, Some("account-b"));
    assert_skipped(&storage, None);
    let initial = [20, 100, 200, 300, 400];
    assert_matching(&storage, &initial, 0.00204);
    assert_matching(&storage, &initial, 0.00204); // unchanged paths and fingerprints

    let auth = Connection::open(home.join("agent/agent.db")).unwrap();
    let set_account = |account| {
        auth.execute(
            "UPDATE auth_credentials SET data = ?1",
            [json!({"accountId":account}).to_string()],
        )
        .unwrap();
    };
    set_account("account-b");
    append(&custom, paid("appended", SECOND, 500));
    assert_skipped(&storage, Some(ACCOUNT)); // neither retained nor appended usage may escape the gate
    set_account(ACCOUNT);
    let after_append = [20, 100, 200, 300, 400, 500];
    assert_matching(&storage, &after_append, 0.00304);
    drop(storage);

    let storage = Storage::open(&database).unwrap();
    assert_matching(&storage, &after_append, 0.00304);
    let cached = |path: &Path, fingerprint: LogFileFingerprint| {
        storage
            .load_log_events("omp", path, fingerprint.size, fingerprint.modified_nanos)
            .unwrap()
    };
    let fingerprint =
        |path: &Path| LogFileFingerprint::from_metadata(&fs::metadata(path).unwrap()).unwrap();
    let retired = [&custom, &child, &fork].map(|path| (path, fingerprint(path)));
    for (path, fingerprint) in retired {
        assert!(cached(path, fingerprint).is_some());
    }

    // Removing a marker stops metering that tree and retires its cache, not its durable facts.
    fs::remove_file(home.join("agent/custom-session-files/custom")).unwrap();
    append(&custom, paid("no-longer-registered", SECOND, 600));
    assert_matching(&storage, &after_append, 0.00304);
    for (path, fingerprint) in retired {
        assert!(cached(path, fingerprint).is_none());
    }
    assert!(cached(&default, fingerprint(&default)).is_some());
}

#[cfg(windows)]
#[test]
fn unreadable_registered_companion_keeps_healthy_usage_and_defers_pruning() {
    use std::os::windows::fs::OpenOptionsExt;

    let directory = tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("omp");
    create_databases(&home, ACCOUNT);
    let default = home.join("agent/sessions/parent.jsonl");
    let retired = root.join("workspace/retired.jsonl");
    let active = root.join("workspace/active.jsonl");
    let companions = active.with_extension("");
    let blocked = companions.join("blocked.jsonl");
    let sibling = companions.join("sibling.jsonl");
    for (path, id, parent, tokens) in [
        (&default, "default", None, 32),
        (&retired, "retired", None, 64),
        (&active, "active", None, 16),
        (&blocked, "blocked", Some("active"), 256),
        (&sibling, "sibling", Some("active"), 8),
    ] {
        write_journal(path, id, STARTED, parent, &[paid(id, FIRST, tokens)]);
    }
    register(&home, "retired", &retired);
    let storage = Storage::open(&root.join("cache.db")).unwrap();
    let pricing = crate::pricing::test_bundled_pricing();
    let scan = || scan_home(&storage, &home, Some(ACCOUNT), now(), &pricing);
    let fingerprint = LogFileFingerprint::from_metadata(&fs::metadata(&retired).unwrap()).unwrap();
    let retired_cache = || {
        storage
            .load_log_events(
                "omp",
                &retired,
                fingerprint.size,
                fingerprint.modified_nanos,
            )
            .unwrap()
    };
    let (outcome, histories) = scan();
    assert_usage(&outcome, &histories, &[32, 64], 0.000192);
    assert!(retired_cache().is_some());

    fs::remove_file(home.join("agent/custom-session-files/retired")).unwrap();
    register(&home, "active", &active);
    register(&home, "blocked", &blocked);
    let exclusive = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&blocked)
        .unwrap();
    append(&default, paid("new-default", SECOND, 128));
    let (outcome, histories) = scan();
    assert_usage(&outcome, &histories, &[8, 16, 32, 64, 128], 0.000496);
    assert!(retired_cache().is_some());

    drop(exclusive);
    let (outcome, histories) = scan();
    assert_usage(&outcome, &histories, &[8, 16, 32, 64, 128, 256], 0.001008);
    assert!(retired_cache().is_none());
}

#[test]
fn isolated_registered_journal_does_not_enroll_unrelated_paths() {
    let directory = tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("omp");
    create_databases(&home, ACCOUNT);
    fs::remove_file(home.join("agent/sessions/parent.jsonl")).unwrap();
    fs::remove_dir(home.join("agent/sessions")).unwrap();

    let isolated = root.join("workspace with spaces/isolated.jsonl");
    let missing_parent = root.join("lost/missing-parent.jsonl");
    write_journal(
        &isolated,
        "isolated",
        STARTED,
        missing_parent.to_str(),
        &[paid("isolated", FIRST, 31)],
    );
    register(&home, "isolated", &isolated);

    let sibling = isolated.with_file_name("sibling.jsonl");
    let other = isolated.with_file_name("other.jsonl");
    let companion = isolated.with_extension("");
    for (path, id, parent) in [
        (sibling.clone(), "sibling", isolated.to_str()),
        (other.clone(), "other", None),
        (companion.join("artifact.jsonl"), "artifact", None),
        (
            companion.join("different-parent.jsonl"),
            "different-parent",
            Some("not-isolated"),
        ),
    ] {
        write_journal(&path, id, STARTED, parent, &[paid(id, FIRST, 999)]);
    }
    let no_header = isolated.with_file_name("not-a-session.jsonl");
    fs::write(&no_header, paid("headerless", FIRST, 999).to_string()).unwrap();
    register(&home, "not-a-session", &no_header);
    register(&home, "stale", &missing_parent);
    register(&home, "directory", isolated.parent().unwrap());
    register(&home, "nested/ignored", &sibling);
    write_marker(
        &home,
        "relative-to-home",
        "../workspace with spaces/sibling.jsonl",
    );
    write_marker(
        &home,
        "relative-to-marker",
        "../../../workspace with spaces/sibling.jsonl",
    );
    write_marker(
        &home,
        "multiple-paths",
        &format!("{}\n{}", sibling.display(), other.display()),
    );

    let storage = Storage::open(&root.join("cache.db")).unwrap();
    let pricing = crate::pricing::test_bundled_pricing();
    let (outcome, histories) = scan_home(&storage, &home, Some(ACCOUNT), now(), &pricing);
    assert_usage(&outcome, &histories, &[31], 0.000062);
}

#[cfg(any(unix, windows))]
#[test]
fn native_session_discovery_does_not_follow_links() {
    let directory = tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("omp");
    create_databases(&home, ACCOUNT);
    let default = home.join("agent/sessions/parent.jsonl");
    write_journal(
        &default,
        "default",
        STARTED,
        None,
        &[paid("default", FIRST, 32)],
    );
    let custom = root.join("workspace/custom.jsonl");
    write_journal(
        &custom,
        "custom",
        STARTED,
        None,
        &[paid("custom", FIRST, 64)],
    );
    register(&home, "custom", &custom);

    let outside = root.join("outside/usage.jsonl");
    write_journal(
        &outside,
        "outside",
        STARTED,
        Some("custom"),
        &[paid("outside", FIRST, 999)],
    );
    let linked_dir = default.parent().unwrap().join("dir-link");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        symlink(outside.parent().unwrap(), &linked_dir).unwrap();
        symlink(&outside, default.parent().unwrap().join("file-link.jsonl")).unwrap();
        let linked_file = root.join("linked.jsonl");
        symlink(&outside, &linked_file).unwrap();
        register(&home, "linked-file", &linked_file);
        let companion = custom.with_extension("");
        fs::create_dir(&companion).unwrap();
        symlink(&outside, companion.join("child-link.jsonl")).unwrap();
        let outside_marker = root.join("marker");
        fs::write(&outside_marker, outside.to_str().unwrap()).unwrap();
        symlink(
            &outside_marker,
            home.join("agent/custom-session-files/linked-marker"),
        )
        .unwrap();
    }
    #[cfg(windows)]
    create_junction(&linked_dir, outside.parent().unwrap());
    let linked_journal = linked_dir.join("usage.jsonl");
    assert_eq!(linked_journal.canonicalize().unwrap(), outside);
    register(&home, "linked-ancestor", &linked_journal);

    let storage = Storage::open(&root.join("cache.db")).unwrap();
    let pricing = crate::pricing::test_bundled_pricing();
    let (outcome, histories) = scan_home(&storage, &home, Some(ACCOUNT), now(), &pricing);
    assert_usage(&outcome, &histories, &[32, 64], 0.000192);
}

#[cfg(windows)]
fn create_junction(link: &Path, target: &Path) {
    use std::{
        os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        },
        ptr::null_mut,
    };
    use windows_sys::Win32::{
        Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        },
        System::IO::DeviceIoControl,
    };

    // MS-FSCC mount-point reparse buffer. Junctions need no symlink privilege.
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00a4;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xa000_0003;
    const NT_PREFIX: &str = r"\??\";
    let target = target.canonicalize().unwrap();
    let name = target.to_str().unwrap().strip_prefix(r"\\?\").unwrap();
    let print_len = u16::try_from(name.encode_utf16().count() * 2).unwrap();
    let substitute_len = print_len + 2 * NT_PREFIX.len() as u16;
    let data_len = 8 + substitute_len + 2 + print_len + 2;
    let mut data = Vec::with_capacity(8 + usize::from(data_len));
    data.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    for field in [
        data_len,
        0,
        0,
        substitute_len,
        substitute_len + 2,
        print_len,
    ] {
        data.extend(field.to_le_bytes());
    }
    for ch in NT_PREFIX
        .encode_utf16()
        .chain(name.encode_utf16())
        .chain([0])
        .chain(name.encode_utf16())
        .chain([0])
    {
        data.extend(ch.to_le_bytes());
    }

    fs::create_dir(link).unwrap();
    let handle = fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(link)
        .unwrap();
    let mut returned = 0;
    // SAFETY: handle is an open synchronous directory handle and all buffers
    // are valid for the call. This control code does not return a data buffer.
    let status = unsafe {
        DeviceIoControl(
            handle.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            data.as_ptr().cast(),
            data.len().try_into().unwrap(),
            null_mut(),
            0,
            &mut returned,
            null_mut(),
        )
    };
    assert_ne!(
        status,
        0,
        "create junction: {}",
        std::io::Error::last_os_error()
    );
    let attributes = fs::symlink_metadata(link).unwrap().file_attributes();
    assert_ne!(attributes & FILE_ATTRIBUTE_REPARSE_POINT, 0);
}

#[test]
fn native_sessions_keep_local_day_and_sol_pricing_contracts() {
    let directory = tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("omp");
    create_databases(&home, ACCOUNT);
    // On Windows this uses the system Local zone too: in UTC the pair must split,
    // while a nonzero offset normally puts both sides of UTC midnight on one local day.
    let after = Utc.with_ymd_and_hms(2026, 7, 12, 0, 0, 0).unwrap();
    let before = after - Duration::seconds(1);
    assert_ne!(before.date_naive(), after.date_naive());
    let before_date = before.with_timezone(&Local).date_naive();
    let after_date = after.with_timezone(&Local).date_naive();
    let same_day = before_date == after_date;
    #[cfg(unix)]
    if std::env::var_os("TOKENLEDGER_TEST_REQUIRE_LOCAL_DAY").is_some() {
        assert!(
            same_day,
            "the isolated TZ must span UTC midnight in one local day"
        );
    }

    let mut short = paid("272000", &before.to_rfc3339(), 262_000);
    short["message"]["usage"]["cacheRead"] = json!(10_000);
    short["message"]["usage"]["output"] = json!(50);
    let mut long = short.clone();
    long["id"] = json!("272001");
    long["timestamp"] = json!(after.to_rfc3339());
    long["message"]["usage"]["input"] = json!(262_001);
    let mut memory = auxiliary("memory", 11, 2);
    memory["purpose"] = json!("memory");
    memory["timestamp"] = json!(after.to_rfc3339());
    memory["usage"].as_object_mut().unwrap().remove("cost");
    let mut image = image_fixture("image", "gpt-image-2-codex", Some("image-gen"), 999);
    image["timestamp"] = json!(after.to_rfc3339());
    let mut foreign = paid("not-codex", &after.to_rfc3339(), 999);
    foreign["message"]["provider"] = json!("openai");
    let mut empty = paid("empty", &after.to_rfc3339(), 0);
    empty["message"]["serviceTier"] = json!("unrecognized");
    empty["message"]["model"] = json!("unknown-zero-token-model");
    empty["message"]["stopReason"] = json!("error");

    write_journal(
        &home.join("agent/sessions/parent.jsonl"),
        "default",
        STARTED,
        None,
        &[short],
    );
    let custom = root.join("workspace/custom.jsonl");
    write_journal(
        &custom,
        "custom",
        STARTED,
        None,
        &[long, memory, image, foreign, empty],
    );
    register(&home, "custom", &custom);

    let storage = Storage::open(&root.join("cache.db")).unwrap();
    for (credits, long_cost) in [(false, 1.050754), (true, 0.525502)] {
        let mut pricing = crate::pricing::test_bundled_pricing();
        pricing.codex_credit_mode = credits;
        let (outcome, histories) = scan_home(&storage, &home, Some(ACCOUNT), after, &pricing);
        let short_cost = 0.5255;
        assert_usage(
            &outcome,
            &histories,
            &[13, 272_050, 272_051],
            short_cost + long_cost,
        );
        for (tokens, cost) in [(272_050, short_cost), (272_051, long_cost), (13, 0.0)] {
            let event = outcome
                .events
                .iter()
                .find(|event| event.total == tokens)
                .unwrap();
            assert_cost(event.cost, cost);
        }
        let daily = &histories[0];
        assert_eq!(daily.daily[0].date, after_date.to_string());
        let today = daily.today.as_ref().unwrap();
        assert_eq!(daily.daily[0].tokens, today.tokens);
        if same_day {
            assert_eq!(daily.daily.len(), 1);
            assert_period(today, 544_114, short_cost + long_cost);
            assert!(daily.yesterday.is_none());
        } else {
            assert_eq!(daily.daily.len(), 2);
            assert_eq!(daily.daily[1].date, before_date.to_string());
            assert_period(today, 272_064, long_cost);
            assert_period(daily.yesterday.as_ref().unwrap(), 272_050, short_cost);
        }
    }
}

#[cfg(unix)]
#[test]
fn local_day_crossing_utc_midnight_is_reproducible_in_both_directions() {
    // Chrono reads TZ on Unix. Use child processes instead of changing it for other tests.
    let (_, module) = module_path!().split_once("::").unwrap();
    let test = format!("{module}::native_sessions_keep_local_day_and_sol_pricing_contracts");
    for zone in ["UTC-8", "UTC+8"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test, "--nocapture"])
            .env("TZ", zone)
            .env("TOKENLEDGER_TEST_REQUIRE_LOCAL_DAY", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("running 1 test"),
            "TZ={zone}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
