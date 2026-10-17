# Development shell for the Tauri trial build on NixOS.
#   nix-shell packages/desktop-tauri/shell.nix
{ pkgs ? import <nixpkgs> { } }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    pkg-config
    gobject-introspection
    rustc
    cargo
    cargo-tauri
    nodejs
    pnpm_10
    # Headless smoke testing: virtual display, input and screenshots.
    xvfb-run
    xdotool
    imagemagick
  ];

  buildInputs = with pkgs; [
    at-spi2-atk
    cairo
    gdk-pixbuf
    glib
    gtk3
    harfbuzz
    librsvg
    libsoup_3
    openssl
    pango
    webkitgtk_4_1
  ];

  shellHook = ''
    # GTK needs its settings schemas, and WebKitGTK needs glib-networking for TLS.
    export XDG_DATA_DIRS=${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}:$XDG_DATA_DIRS
    export GIO_MODULE_DIR=${pkgs.glib-networking}/lib/gio/modules/
  '';
}
