{
  description = "Sanlys/tools: desktop (native, dynamically-linked) builds of every egui-based app in this workspace";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # eframe (every app below is egui/eframe -- see the root Cargo.toml's
      # `eframe = { features = ["glow", "wayland", "x11", ...] }`) loads its
      # window-system and GL backend at *runtime* via dlopen rather than
      # linking them at build time, so nixpkgs' automatic RPATH patching
      # never sees them -- they have to be added to LD_LIBRARY_PATH by hand,
      # same as any other winit/wgpu/glow Rust GUI app on NixOS.
      runtimeLibs = pkgs: with pkgs; [
        libxkbcommon
        wayland
        libGL
        xorg.libX11
        xorg.libXcursor
        xorg.libXi
        xorg.libXrandr
      ];

      # crates/adapters/auth's native login flow (used by hello-standalone,
      # game-mgr-standalone, and portal-desktop -- NOT game-mgr-client,
      # which has its own file-based token store instead, see
      # apps/game-mgr/core/src/oidc.rs) pulls in the `keyring` crate's
      # secret-service backend, which -- unlike `rfd`'s file-picker portal,
      # which talks to D-Bus via the pure-Rust `zbus` and needs nothing
      # extra here -- links against the real C `libdbus` at build time
      # (`dbus` -> `libdbus-sys` -> `pkg-config`), so it needs both
      # `pkg-config`+`dbus` to build and `dbus`'s shared library at runtime.
      dbusRuntimeLibs = pkgs: [ pkgs.dbus ];

      # One derivation shape for every native egui app in this workspace.
      # `pname` doubles as both the Nix package name and the built binary's
      # name (every one of these packages has exactly one [[bin]] target
      # matching its own binary name), so `nix run .#<pname>` and
      # `nix build .#<pname>` both work with no extra `apps` output needed.
      mkEguiApp = pkgs:
        { pname
        , cargoBuildFlags
        , needsDbus ? false
        , extraPath ? [ ]
        }:
        pkgs.rustPlatform.buildRustPackage {
          inherit pname cargoBuildFlags;
          version = "0.1.0";
          src = self;
          # Single workspace-wide lockfile at the repo root -- every crate
          # here is a crates.io or in-workspace path dependency (no git
          # deps), so no `outputHashes` are needed.
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.makeWrapper ]
            ++ pkgs.lib.optional needsDbus pkgs.pkg-config;
          buildInputs = pkgs.lib.optional needsDbus pkgs.dbus;

          # No test suite in this workspace can run inside the Nix build
          # sandbox (network/DB/GUI needed) -- these are all covered by
          # `cargo test --workspace` in CI instead, see docs/local-development.md.
          doCheck = false;

          postFixup = ''
            wrapProgram $out/bin/${pname} \
              ${pkgs.lib.optionalString (extraPath != [ ])
                "--prefix PATH : ${pkgs.lib.makeBinPath extraPath}"} \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath
                (runtimeLibs pkgs ++ pkgs.lib.optionals needsDbus (dbusRuntimeLibs pkgs))}
          '';
        };
    in
    {
      packages = forAllSystems (pkgs: rec {
        # apps/game-mgr/client -- the actual desktop app: install/launch/
        # sync management for a personal game library (talks to Proton/umu,
        # Steam, the filesystem, and game-mgr-backend's API directly). See
        # docs/architecture.md and apps/game-mgr/client's module docs.
        game-mgr-client = mkEguiApp pkgs {
          pname = "game-mgr-client";
          cargoBuildFlags = [ "-p" "game-mgr-client" ];
          # innoextract: InnoSetup installer extraction (steps/extract.rs).
          # umu-launcher: provides `umu-run`, used to launch games through
          # Proton (run.rs). Neither is dlopen'd -- both are looked up on
          # PATH at runtime (`which`), so this is a PATH prefix, not
          # LD_LIBRARY_PATH.
          extraPath = [ pkgs.innoextract pkgs.umu-launcher ];
        };

        # apps/game-mgr/frontend's own standalone build -- the same
        # GameMgrPanel the portal embeds, run on its own talking to
        # game-mgr-backend's HTTP API. Distinct from game-mgr-client above,
        # which manages installs/launches locally instead.
        game-mgr-standalone = mkEguiApp pkgs {
          pname = "game-mgr-standalone";
          cargoBuildFlags = [ "-p" "game-mgr-frontend" "--bin" "game-mgr-standalone" ];
          needsDbus = true;
        };

        # apps/hello/frontend's standalone build -- the reference example
        # tool's own native window (apps/hello/frontend/src/bin/standalone.rs).
        hello-standalone = mkEguiApp pkgs {
          pname = "hello-standalone";
          cargoBuildFlags = [ "-p" "hello-frontend" "--bin" "hello-standalone" ];
          needsDbus = true;
        };

        # apps/portal/frontend's native desktop build -- the unified portal
        # (every panel + sign-in) as a window instead of a browser tab. See
        # apps/portal/frontend/src/bin/desktop.rs's module doc comment.
        portal-desktop = mkEguiApp pkgs {
          pname = "portal-desktop";
          cargoBuildFlags = [ "-p" "portal-frontend" "--bin" "portal-desktop" ];
          needsDbus = true;
        };

        default = game-mgr-client;
      });
    };
}
