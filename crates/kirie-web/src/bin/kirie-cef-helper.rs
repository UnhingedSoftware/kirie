fn main() {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    let args = cef::args::Args::new();
    let mut app = kirie_web::cef::app::make_app();

    let code = cef::execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());

    std::process::exit(if code >= 0 { code } else { 0 });
}
