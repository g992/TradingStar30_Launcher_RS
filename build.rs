extern crate embed_resource;

fn main() {
    // Компилируем файл ресурсов только для Windows MSVC таргета
    if std::env::var("TARGET").unwrap().contains("windows-msvc") {
        println!("cargo:rerun-if-changed=app.rc"); // Перекомпилировать build.rs, если app.rc изменился
        embed_resource::compile("app.rc", embed_resource::NONE);
    }
}
