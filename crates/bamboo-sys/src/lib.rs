//! Обёртки над системными API Windows.
//!
//! Единственный крейт Bamboo, где разрешён `unsafe`. Во всех остальных стоит
//! `#![forbid(unsafe_code)]` — это делает аудит выполнимым: чтобы проверить
//! проект на корректность работы с памятью, достаточно прочитать один крейт.
//!
//! Наружу отдаются только безопасные типы из `bamboo-core`.

#![cfg(windows)]

pub mod apps;
pub mod boot;
pub mod budget;
pub mod clock;
pub mod cmdline;
pub mod control;
pub mod cpu;
pub mod digest;
pub mod etw;
pub mod eventlog;
pub mod extensions;
pub mod freeze;
pub mod frequency;
mod games;
mod gpu;
mod http;
pub mod inventory;
mod iolimit;
pub mod memory;
pub mod notify;
pub mod nt;
pub mod paging;
pub mod pdh;
pub mod pipe;
pub mod power;
pub mod process;
pub mod restore;
pub mod schedtask;
pub mod service;
pub mod services;
mod settings;
pub mod startup;
pub mod storage;
pub mod user;
pub mod wake;
pub mod window;

pub use apps::installed_applications;
pub use boot::{boot_culprits, boot_history, BootCulprit, BootRecord};
pub use budget::{apply_self_limits, own_memory, OwnMemory};
pub use clock::{monotonic_ms, now};
pub use cmdline::{browser_role, command_line, BrowserRole};
pub use cpu::CpuTimesBuffer;
pub use digest::sha256_hex;
pub use eventlog::daily_error_count;
pub use extensions::{installed as installed_extensions, Extension};
pub use games::{installed as installed_games, Game};
pub use gpu::{load_by_process as gpu_load_by_process, GpuCounter, GpuLoad};
pub use http::{fetch, fetch_text};
pub use inventory::{drivers, scheduled_tasks, Driver, ScheduledTask};
pub use iolimit::{IoLimit, LimitedProcess};
pub use memory::{memory_stat, system_counts, SystemCounts};
pub use notify::{Importance, Notifier};
pub use power::{power_capabilities, power_status, PowerCapabilities, PowerSource, PowerStatus};
pub use process::{
    has_hung_window, hung_process_ids, terminate as terminate_process, ProcessBuffer, ProcessIter,
    RawProcess,
};
pub use restore::{
    create_restore_point, has_recent_restore_point, last_restore_point_ms, RestoreOutcome,
};
pub use schedtask::{channel_enabled, recent_task_starts, started_by_task, StartedByTask};
pub use service::has_start_trigger;
pub use service::{
    install as install_service, service_start, set_service_start, uninstall as uninstall_service,
    ServiceStart, StopSignal,
};
pub use services::{service_by_pid, service_names, stop_service, ServiceOwner};
pub use settings::{
    autopilot_enabled, language, set_autopilot_enabled, set_language, set_show_widget_on_start,
    show_widget_on_start,
};
pub use startup::{
    add_to_startup, is_elevated, is_in_startup, is_scheduled_at_logon, remove_from_startup,
    remove_startup_command, schedule_at_logon, set_startup_command, set_startup_enabled,
    startup_command, unschedule_at_logon, user_startup_items, what_needs_elevation, StartupItem,
};
pub use storage::{enumerate as enumerate_drives, read_smart, Drive};
pub use user::{double_click_time_ms, idle_ms, notification_state, NotificationState};
pub use wake::{wake_history, WakeEvent, WakeSource};
pub use window::window_titles;
