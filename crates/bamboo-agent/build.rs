fn main() {
    // Один входной файл импортирует оба окна: и виджет, и главное окно
    // попадают в одну генерацию.
    //
    // Переводы вкомпилируются в исполняемый файл, а не читаются gettext
    // во время работы. Это важно: фича slint/gettext на Windows не делает
    // ничего — её код целиком под cfg(unix). Встроенные переводы —
    // другой механизм, и он работает везде.
    // Без этого правка перевода не пересобирается: cargo следит за .slint,
    // но про каталог переводов не знает, и в файл молча уезжает прежний
    // текст. Проверено — так и было.
    println!("cargo:rerun-if-changed=translations");

    let config =
        slint_build::CompilerConfiguration::new().with_bundled_translations("translations");
    slint_build::compile_with_config("ui/app.slint", config).expect("не удалось собрать интерфейс");
}
