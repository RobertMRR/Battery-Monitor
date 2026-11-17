//! Embeds the Windows resources: app icon, version info (Explorer › Properties › Details) and the app manifest.
//! Needs rc.exe from the Windows SDK (present with Visual Studio's C++ workload and on GitHub's Windows runners).

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
                        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-env-changed=BM_VERSION");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Same rule as `VERSION` in main.rs: release builds set BM_VERSION, local builds are "<Cargo version>-dev".
    let version = std::env::var("BM_VERSION").unwrap_or_else(|_| format!("{}-dev", env!("CARGO_PKG_VERSION")));
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/app.ico")
        .set("ProductName", "Battery Monitor")
        .set("FileDescription", "Battery Monitor")
        .set("ProductVersion", &version)
        .set("OriginalFilename", "BatteryMonitor.exe")
        .set("LegalCopyright", "Copyright (C) 2026 RobertMRR. Licensed under GPL-3.0-or-later.")
        .set_manifest(MANIFEST);
    if let Err(e) = res.compile() {
        panic!("compiling Windows resources failed (rc.exe from the Windows SDK is required): {e}");
    }
}
