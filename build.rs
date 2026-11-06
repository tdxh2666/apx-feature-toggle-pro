use std::path::Path;

fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();

        // 如果有图标就嵌入，没有也不会导致编译失败
        if Path::new("resources/app.ico").exists() {
            res.set_icon("resources/app.ico");
        }

        // 方案 A：自动请求管理员权限
        res.set_manifest(r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#);

        if let Err(e) = res.compile() {
            eprintln!("Warning: failed to compile Windows resources: {}", e);
        }
    }
}
