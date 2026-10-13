#!/usr/bin/env bash
set -e

# OpenDictate — скрипт удаления.

if [ "$(id -u)" -eq 0 ]; then
    PREFIX="/usr/local"
    APP_DIR="/usr/local/share/applications"
    ICON_BASE="/usr/local/share/icons/hicolor"
else
    PREFIX="${HOME}/.local"
    APP_DIR="${HOME}/.local/share/applications"
    ICON_BASE="${HOME}/.local/share/icons/hicolor"
fi

echo "=== Удаление OpenDictate из ${PREFIX} ==="
rm -f "${PREFIX}/bin/opendictate" "${PREFIX}/bin/opendictate-gui"
rm -f "${APP_DIR}/opendictate.desktop" "${APP_DIR}/opendictate-gui.desktop"
for s in scalable 48x48 64x64 128x128; do
    case "$s" in
        scalable) ext=svg ;;
        *) ext=png ;;
    esac
    rm -f "${ICON_BASE}/${s}/apps/opendictate.${ext}"
done

if [ -f /etc/udev/rules.d/90-opendictate.rules ]; then
    if [ "$(id -u)" -eq 0 ]; then
        rm -f /etc/udev/rules.d/90-opendictate.rules
        udevadm trigger /dev/uinput 2>/dev/null || true
        echo "udev-правило удалено."
    else
        echo "[sudo] Для удаления udev-правила: sudo rm /etc/udev/rules.d/90-opendictate.rules"
    fi
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${APP_DIR}" 2>/dev/null || true
fi

echo "OpenDictate удалён."
