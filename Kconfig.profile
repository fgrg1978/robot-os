# Kconfig.profile — Deployment profile
#
# RFC-0026 Phase C1.  Selects the cap-size tier.  Individual caps in
# Kconfig.limits can be overridden after choosing a profile.

menu "Deployment Profile"

choice
    prompt "Deployment profile"
    default PROFILE_EDGE
    help
      Selects the default static resource cap sizes.  See RFC-0023 for the
      full table.  Individual caps in "Resource Limits" can be overridden
      after choosing a profile, but choose the closest base first to
      minimise manual editing.

    config PROFILE_EMBEDDED
        bool "Embedded — microcontrollers (<1 MiB SRAM)"
        help
          For tiny SoCs with very limited SRAM.  Designed for a single
          robot instance running a minimal kernel.  No fleet support;
          no ML inference; minimal IPC.  Compatible with NO_MMU.

    config PROFILE_EDGE
        bool "Edge — single-robot SBC (VF2 / K1 / RK3588)"
        help
          Default profile.  Sized for ~200 userspace apps + 100 sensor
          streams + brain link + OTA.  Memory budget ~45 MiB.
          Targets single-board computers with 1-8 GiB RAM.

    config PROFILE_FLEET
        bool "Fleet — gateway / edge-server (many robots aggregated)"
        help
          For a PHANES instance acting as a gateway aggregating multiple
          downstream robots.  Large cap tables, high TCP connection
          count, multi-stream OTA.  Memory budget ~256 MiB.
          Incompatible with NO_MMU and PROFILE_EMBEDDED.

endchoice

endmenu # Deployment Profile
