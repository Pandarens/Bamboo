//! Интеграционные проверки хранилища на настоящей базе.

use bamboo_core::Bytes;
use bamboo_store::{
    into_buckets, BootEntry, Level, SamplePoint, SmartSnapshot, Store, L2_BUCKET_MS,
};

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;

fn store() -> Store {
    Store::in_memory().expect("база в памяти не открылась")
}

fn point(at_ms: i64, cpu_ms: u32, private_kib: u32) -> SamplePoint {
    SamplePoint {
        at_unix_ms: at_ms,
        cpu_ms,
        read_kib: 5,
        write_kib: 15,
        private_kib,
        working_set_kib: private_kib / 2,
        handles: 250,
        threads: 8,
    }
}

#[test]
fn fresh_database_is_migrated() {
    let store = store();
    assert_eq!(store.schema_version().unwrap(), 1);
}

#[test]
fn opening_twice_does_not_re_run_migrations() {
    let dir = std::env::temp_dir().join("bamboo-test-migrate");
    let _ = std::fs::remove_file(&dir);

    {
        let first = Store::open(&dir).unwrap();
        first.set_pref("проверка", "значение").unwrap();
    }
    {
        let second = Store::open(&dir).unwrap();
        assert_eq!(second.schema_version().unwrap(), 1);
        assert_eq!(
            second.pref("проверка").unwrap().as_deref(),
            Some("значение"),
            "повторное открытие снесло данные"
        );
    }

    let _ = std::fs::remove_file(&dir);
}

/// Повреждённая база не останавливает наблюдение навсегда.
///
/// Взято с живой машины: после недели работы база оказалась повреждена,
/// история молча перестала собираться, а разделы показывали пустоту,
/// неотличимую от «ещё не накопилось». Файл здесь портится по-настоящему —
/// поверх страниц пишется мусор, — а не подсовывается заглушка: проверять
/// надо тот самый путь, которым SQLite сообщает о битом дереве.
#[test]
fn a_corrupted_database_is_set_aside_and_rebuilt() {
    use std::io::{Seek, SeekFrom, Write};

    let dir = std::env::temp_dir().join("bamboo-test-corrupt");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("наблюдения.db");

    // Наполняем настолько, чтобы страниц стало много: порча одной страницы
    // в почти пустой базе может не задеть ничего живого.
    {
        let store = Store::open(&path).unwrap();
        for number in 0..400 {
            store
                .app_id(&format!("c:\\программа{number}.exe:?"), "п.exe", 1000)
                .unwrap();
        }
    }

    // Портим середину файла. Заголовок не трогаем намеренно: испорченный
    // заголовок даёт «не база данных», а это другая, более простая ошибка.
    let size = std::fs::metadata(&path).unwrap().len();
    {
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(size / 2)).unwrap();
        file.write_all(&[0x5A; 4096]).unwrap();
    }

    let store = Store::open(&path).expect("повреждение не должно ронять Bamboo");
    assert_eq!(
        store.schema_version().unwrap(),
        1,
        "после замены база обязана быть рабочей"
    );
    assert!(
        dir.join("наблюдения.повреждена.db").exists(),
        "повреждённый файл должен остаться рядом — по нему разбираются"
    );
    // И новая база действительно пуста и пригодна к записи.
    store.app_id("c:\\снова.exe:?", "снова.exe", 2000).unwrap();

    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn app_key_is_stable_across_restarts() {
    let store = store();
    let first = store.app_id("c:\\app.exe:?", "app.exe", 1000).unwrap();
    let again = store.app_id("c:\\app.exe:?", "app.exe", 2000).unwrap();
    assert_eq!(first, again, "один ключ должен давать один идентификатор");

    let other = store.app_id("c:\\other.exe:?", "other.exe", 1000).unwrap();
    assert_ne!(first, other);
}

#[test]
fn buckets_survive_a_write_and_read_round_trip() {
    let mut store = store();
    let app = store.app_id("app", "app.exe", 0).unwrap();

    let points: Vec<SamplePoint> = (0..60).map(|i| point(i * 60_000, 500, 100_000)).collect();
    let buckets = into_buckets(&points, L2_BUCKET_MS);
    store.write_buckets(Level::L2, app, &buckets).unwrap();

    let read = store.read_buckets(Level::L2, app, 0, DAY).unwrap();
    assert_eq!(read, buckets);
}

#[test]
fn writing_the_same_bucket_twice_accumulates() {
    let mut store = store();
    let app = store.app_id("app", "app.exe", 0).unwrap();

    // Служба перезапустилась и дописала тот же интервал второй порцией.
    let first = into_buckets(&[point(0, 500, 100_000)], L2_BUCKET_MS);
    let second = into_buckets(&[point(60_000, 300, 900_000)], L2_BUCKET_MS);

    store.write_buckets(Level::L2, app, &first).unwrap();
    store.write_buckets(Level::L2, app, &second).unwrap();

    let read = store.read_buckets(Level::L2, app, 0, DAY).unwrap();
    assert_eq!(read.len(), 1, "интервал должен остаться одним");
    assert_eq!(read[0].samples, 2);
    assert_eq!(read[0].cpu_ms, 800, "процессорное время потерялось");
    assert_eq!(read[0].private_kib.max, 900_000, "максимум не обновился");
    assert_eq!(read[0].private_kib.min, 100_000);
}

#[test]
fn old_data_moves_to_the_hourly_level() {
    let mut store = store();
    let app = store.app_id("app", "app.exe", 0).unwrap();
    let now = 40 * DAY;

    // Данные месячной давности: 35 суток назад, по замеру каждые 15 минут.
    let start = now - 35 * DAY;
    let points: Vec<SamplePoint> = (0..96)
        .map(|i| point(start + i * 15 * 60_000, 100, 50_000))
        .collect();
    let buckets = into_buckets(&points, L2_BUCKET_MS);
    store.write_buckets(Level::L2, app, &buckets).unwrap();

    let created = store.roll_up_to_l3(now).unwrap();
    assert_eq!(created, 24, "сутки должны стать двадцатью четырьмя часами");

    let hourly = store.read_buckets(Level::L3, app, 0, now).unwrap();
    let cpu_l2: u64 = buckets.iter().map(|b| b.cpu_ms).sum();
    let cpu_l3: u64 = hourly.iter().map(|b| b.cpu_ms).sum();
    assert_eq!(cpu_l2, cpu_l3, "при подъёме потерялось процессорное время");
}

#[test]
fn pruning_respects_each_levels_depth() {
    let mut store = store();
    let app = store.app_id("app", "app.exe", 0).unwrap();
    let now = 400 * DAY;

    let fresh = into_buckets(&[point(now - DAY, 100, 1000)], L2_BUCKET_MS);
    let stale = into_buckets(&[point(now - 60 * DAY, 100, 1000)], L2_BUCKET_MS);
    store.write_buckets(Level::L2, app, &fresh).unwrap();
    store.write_buckets(Level::L2, app, &stale).unwrap();

    let ancient = into_buckets(&[point(now - 400 * DAY, 100, 1000)], L2_BUCKET_MS);
    store.write_buckets(Level::L3, app, &ancient).unwrap();

    store.prune(now).unwrap();

    let l2 = store
        .read_buckets(Level::L2, app, i64::MIN, i64::MAX)
        .unwrap();
    assert_eq!(l2.len(), 1, "из L2 должно было уйти всё старше 30 суток");

    let l3 = store
        .read_buckets(Level::L3, app, i64::MIN, i64::MAX)
        .unwrap();
    assert!(l3.is_empty(), "из L3 должно было уйти всё старше года");
}

#[test]
fn daily_write_rate_is_computed_from_smart_history() {
    let store = store();
    let start = 100 * DAY;

    // За семь суток накопитель принял 105 ГБ — это 15 ГБ в сутки.
    for day in 0..8 {
        store
            .record_smart(
                "накопитель-1",
                &SmartSnapshot {
                    at_unix_ms: start + day * DAY,
                    data_written: Some(Bytes(day as u64 * 15_000_000_000)),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    let rate = store
        .daily_write_average("накопитель-1", start)
        .unwrap()
        .unwrap();
    let gigabytes = rate.as_u64() as f64 / 1e9;
    assert!(
        (gigabytes - 15.0).abs() < 0.1,
        "получилось {gigabytes:.2} ГБ в сутки"
    );
}

#[test]
fn a_single_smart_reading_gives_no_rate() {
    let store = store();
    store
        .record_smart(
            "накопитель-1",
            &SmartSnapshot {
                at_unix_ms: 0,
                data_written: Some(Bytes(1_000_000)),
                ..Default::default()
            },
        )
        .unwrap();

    // По одной точке темп записи не считается, и выдумывать его нельзя.
    assert_eq!(store.daily_write_average("накопитель-1", 0).unwrap(), None);
}

#[test]
fn readings_too_close_together_give_no_rate() {
    let store = store();
    for minute in 0..3 {
        store
            .record_smart(
                "накопитель-1",
                &SmartSnapshot {
                    at_unix_ms: minute * 60_000,
                    data_written: Some(Bytes(minute as u64 * 1_000_000_000)),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    // Три минуты наблюдений — экстраполировать на сутки нельзя.
    assert_eq!(store.daily_write_average("накопитель-1", 0).unwrap(), None);
}

#[test]
fn growing_media_errors_are_detected() {
    let store = store();
    for (index, errors) in [0u64, 0, 2].iter().enumerate() {
        store
            .record_smart(
                "накопитель-1",
                &SmartSnapshot {
                    at_unix_ms: index as i64 * HOUR,
                    media_errors: Some(*errors),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    assert!(store.media_errors_grew("накопитель-1", 0).unwrap());

    store
        .record_smart(
            "накопитель-2",
            &SmartSnapshot {
                at_unix_ms: 0,
                media_errors: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!store.media_errors_grew("накопитель-2", 0).unwrap());
}

#[test]
fn boot_history_comes_back_newest_first() {
    let store = store();
    for day in 0..5 {
        store
            .record_boot(&BootEntry {
                at_unix_ms: day * DAY,
                total_ms: 30_000 + day as u64 * 1000,
                main_path_ms: 15_000,
                post_boot_ms: 15_000,
                degradation_ms: None,
            })
            .unwrap();
    }

    let history = store.boot_history(3).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].at_unix_ms, 4 * DAY);
    assert!(history[0].at_unix_ms > history[1].at_unix_ms);
}

#[test]
fn the_same_boot_is_not_recorded_twice() {
    let store = store();
    let entry = BootEntry {
        at_unix_ms: DAY,
        total_ms: 30_000,
        main_path_ms: 15_000,
        post_boot_ms: 15_000,
        degradation_ms: None,
    };
    store.record_boot(&entry).unwrap();
    store.record_boot(&entry).unwrap();

    assert_eq!(store.boot_history(10).unwrap().len(), 1);
}

#[test]
fn a_month_of_data_fits_the_disk_budget() {
    let mut store = store();
    let now = 30 * DAY;

    // 150 приложений, каждое с историей за 30 суток по интервалу в 15 минут.
    // Это заметно больше, чем реально держится в таблице: большинство
    // процессов живёт минуты, а не месяц.
    for app_index in 0..150 {
        let app = store
            .app_id(&format!("app-{app_index}"), "app.exe", 0)
            .unwrap();
        let buckets: Vec<_> = (0..(30 * 96))
            .map(|i| bamboo_store::Bucket {
                start_ms: i * L2_BUCKET_MS,
                samples: 180,
                cpu_ms: 5000,
                read_kib: 100,
                write_kib: 200,
                ..Default::default()
            })
            .collect();
        store.write_buckets(Level::L2, app, &buckets).unwrap();
    }

    store.prune(now).unwrap();
    let size = store.size_bytes().unwrap();

    // Бюджет из раздела 4 ТЗ — не больше 200 МБ на диске.
    assert!(
        size < Bytes::from_mib(200),
        "база заняла {size} при лимите 200 МБ"
    );
}

#[test]
fn preferences_round_trip() {
    let store = store();
    assert_eq!(store.pref("нет-такого").unwrap(), None);

    store.set_pref("профиль", "Игра").unwrap();
    assert_eq!(store.pref("профиль").unwrap().as_deref(), Some("Игра"));

    store.set_pref("профиль", "Обычный").unwrap();
    assert_eq!(store.pref("профиль").unwrap().as_deref(), Some("Обычный"));
}
