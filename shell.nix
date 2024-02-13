with import <unstable> {};
mkShell {
  name = "ztui";
  buildInputs = with pkgs; [
    cacert
    cargo
    curl
    openssl
    perl
    pkg-config
    rustc
  ];

  PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkg-config";
  RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
}
