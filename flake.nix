{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        isDarwin = pkgs.stdenv.isDarwin;

      in {
        # ── Dev shell ──────────────────────────────────────────────
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustToolchain
            pkg-config
            cmake
          ];

          buildInputs = with pkgs; [
            sqlite
          ]
          ++ lib.optionals isDarwin [ libiconv ]
          ++ lib.optionals (!isDarwin) [ openssl ];

          env = pkgs.lib.optionalAttrs isDarwin {
            # Point the C toolchain at the Xcode CLI tools SDK so
            # ring / aws-lc-sys / libsqlite3-sys can find system
            # frameworks (CoreServices, Security, etc.) and libiconv.
            SDKROOT = "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk";
          };
        };

        # ── Demo shell (dev shell + caddy + curl for health checks) ─
        devShells.demo = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustToolchain
            pkg-config
            cmake
            caddy
            curl
          ];

          buildInputs = with pkgs; [
            sqlite
          ]
          ++ lib.optionals isDarwin [ libiconv ]
          ++ lib.optionals (!isDarwin) [ openssl ];

          env = pkgs.lib.optionalAttrs isDarwin {
            SDKROOT = "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk";
          };

          shellHook = ''
            echo ""
            echo "TeleICU Gateway — demo shell"
            echo "Run:  ./dev/demo.sh"
            echo ""
          '';
        };

        # ── Demo launcher (nix run .#demo) ─────────────────────────
        # Runs the gateway behind Caddy with self-signed HTTPS.
        # Prefers a local cargo-built binary to avoid slow Nix rebuilds;
        # falls back to `nix build` only if no local binary exists.
        packages.demo =
          let nixBin = "${self.packages.${system}.default}/bin/teleicu-gateway";
          in pkgs.writeShellApplication {
          name = "teleicu-gateway-demo";
          runtimeInputs = with pkgs; [ caddy curl coreutils gnused ];
          text = ''
            # Prefer local cargo-built binary (instant if unchanged),
            # fall back to the Nix-built one (hermetic but slower).
            LOCAL_BIN="./target/release/teleicu-gateway"
            NIX_BIN="${nixBin}"
            if [ -x "$LOCAL_BIN" ]; then
              GATEWAY_BIN="$LOCAL_BIN"
              echo "Using local binary: $LOCAL_BIN"
            else
              GATEWAY_BIN="$NIX_BIN"
              echo "No local binary found — using Nix-built binary"
            fi
            export PATH="${pkgs.lib.makeBinPath (with pkgs; [ caddy curl coreutils gnused ])}:$PATH"

            DEMO_PORT="''${DEMO_PORT:-8443}"
            GATEWAY_PORT="''${BIND_PORT:-8090}"
            WORK_DIR="''${TMPDIR:-/tmp}/teleicu-demo-$$"
            mkdir -p "$WORK_DIR/caddy_data" "$WORK_DIR/caddy_config"

            cleanup() {
              echo ""
              echo "── Shutting down ──"
              [ -n "''${CADDY_PID:-}" ] && kill "$CADDY_PID" 2>/dev/null || true
              [ -n "''${GW_PID:-}" ] && kill "$GW_PID" 2>/dev/null || true
              wait 2>/dev/null || true
              rm -rf "$WORK_DIR"
              echo "Done."
            }
            trap cleanup EXIT INT TERM

            # Generate Caddyfile
            cat > "$WORK_DIR/Caddyfile" <<CADDY
            localhost:$DEMO_PORT {
              tls internal
              reverse_proxy localhost:$GATEWAY_PORT {
                header_up Host {host}
                header_up X-Real-IP {remote_host}
                header_up X-Forwarded-For {remote_host}
                header_up X-Forwarded-Proto {scheme}
              }
            }
            CADDY

            # Start gateway
            export BIND_HOST="''${BIND_HOST:-127.0.0.1}"
            export BIND_PORT="$GATEWAY_PORT"
            [ -z "''${GATEWAY_DEVICE_ID:-}" ] && export GATEWAY_DEVICE_ID="demo-gateway"
            "$GATEWAY_BIN" &
            GW_PID=$!

            echo -n "Waiting for gateway"
            for _ in $(seq 1 30); do
              curl -sf "http://127.0.0.1:$GATEWAY_PORT/health/ping" >/dev/null 2>&1 && break
              kill -0 "$GW_PID" 2>/dev/null || { echo " FAILED"; exit 1; }
              echo -n "."
              sleep 0.5
            done
            echo " ready!"

            # Start Caddy
            XDG_DATA_HOME="$WORK_DIR/caddy_data" \
            XDG_CONFIG_HOME="$WORK_DIR/caddy_config" \
              caddy run --config "$WORK_DIR/Caddyfile" --adapter caddyfile &
            CADDY_PID=$!
            sleep 1

            echo ""
            echo "════════════════════════════════════════════════════════"
            echo "  TeleICU Gateway Demo"
            echo ""
            echo "  HTTPS : https://localhost:$DEMO_PORT"
            echo "  HTTP  : http://localhost:$GATEWAY_PORT  (direct)"
            echo ""
            echo "  Health: https://localhost:$DEMO_PORT/health/status"
            echo "  Ping  : https://localhost:$DEMO_PORT/health/ping"
            echo ""
            echo "  Self-signed cert — browser will warn, accept to proceed"
            echo "  Press Ctrl+C to stop"
            echo "════════════════════════════════════════════════════════"
            echo ""

            wait -n "$GW_PID" "$CADDY_PID" 2>/dev/null || true
          '';
        };

        # ── Production package (nix build) ─────────────────────────
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "teleicu-gateway";
          version =
            (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [ pkg-config cmake ];
          buildInputs = with pkgs; [ sqlite ]
            ++ lib.optionals isDarwin [ libiconv ]
            ++ lib.optionals (!isDarwin) [ openssl ];
        };
      }
    ) // {
      # ── NixOS module (Linux deployment only) ─────────────────────
      nixosModules.default = { config, pkgs, lib, ... }:
        let
          cfg = config.services.teleicu-gateway;
          rtsptoweb = pkgs.fetchurl {
            url =
              "https://github.com/deepch/RTSPtoWeb/releases/download/v0.0.10/RTSPtoWeb_linux_amd64.tar.gz";
            hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
          };
        in {
          options.services.teleicu-gateway = {
            enable = lib.mkEnableOption "TeleICU Gateway";
            environmentFile = lib.mkOption { type = lib.types.path; };
            rtsptowebConfigFile = lib.mkOption { type = lib.types.path; };
          };

          config = lib.mkIf cfg.enable {
            systemd.services.rtsptoweb = {
              description = "RTSPtoWeb stream server";
              wantedBy = [ "multi-user.target" ];
              after = [ "network.target" ];
              serviceConfig = {
                ExecStart =
                  "${rtsptoweb}/RTSPtoWeb --config ${cfg.rtsptowebConfigFile}";
                Restart = "on-failure";
                RestartSec = "3s";
                DynamicUser = true;
              };
            };

            systemd.services.teleicu-gateway = {
              description = "TeleICU Gateway";
              wantedBy = [ "multi-user.target" ];
              after = [ "network.target" "rtsptoweb.service" ];
              serviceConfig = {
                ExecStart =
                  "${self.packages.${pkgs.system}.default}/bin/teleicu-gateway";
                EnvironmentFile = cfg.environmentFile;
                Restart = "on-failure";
                RestartSec = "5s";
                DynamicUser = true;
                StateDirectory = "teleicu-gateway";
                WorkingDirectory = "/var/lib/teleicu-gateway";
                PrivateTmp = true;
                NoNewPrivileges = true;
                ProtectSystem = "strict";
                ProtectHome = true;
                ReadWritePaths = [ "/var/lib/teleicu-gateway" ];
              };
            };
          };
        };
    };
}
