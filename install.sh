#!/usr/bin/env bash
set -e

# OpenDictate — Установочный скрипт для Linux (ALT Linux и др.)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

if [ "$(id -u)" -eq 0 ]; then
    PREFIX="/usr/local"
else
    PREFIX="${HOME}/.local"
fi

BIN_DIR="${PREFIX}/bin"
APP_DIR="${PREFIX}/share/applications"
ICON_BASE="${PREFIX}/share/icons/hicolor"

echo "==============================================="
echo "   Установка OpenDictate (директория: ${PREFIX})"
echo "==============================================="

echo "-> Сборка оптимизированного релизного бинарника..."
cargo build --release

echo "-> Копирование исполняемого файла в ${BIN_DIR}..."
mkdir -p "${BIN_DIR}"
install -m 755 target/release/opendictate "${BIN_DIR}/opendictate"

echo "-> Установка иконок приложения..."
mkdir -p "${ICON_BASE}/scalable/apps"
mkdir -p "${ICON_BASE}/48x48/apps"
mkdir -p "${ICON_BASE}/64x64/apps"
mkdir -p "${ICON_BASE}/128x128/apps"

install -m 644 assets/opendictate.svg "${ICON_BASE}/scalable/apps/opendictate.svg"
[ -f assets/opendictate_48.png ] && install -m 644 assets/opendictate_48.png "${ICON_BASE}/48x48/apps/opendictate.png"
[ -f assets/opendictate_64.png ] && install -m 644 assets/opendictate_64.png "${ICON_BASE}/64x64/apps/opendictate.png"
[ -f assets/opendictate_128.png ] && install -m 644 assets/opendictate_128.png "${ICON_BASE}/128x128/apps/opendictate.png"

echo "-> Установка десктоп-ярлыка меню..."
mkdir -p "${APP_DIR}"
install -m 644 assets/opendictate.desktop "${APP_DIR}/opendictate.desktop"
sed -i "s|^Exec=.*|Exec=${BIN_DIR}/opendictate|" "${APP_DIR}/opendictate.desktop"

echo "-> Обновление баз данных окружения..."
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${APP_DIR}" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "${ICON_BASE}" 2>/dev/null || true
fi

echo ""
echo "Установка успешно завершена!"
echo "Бинарник:  ${BIN_DIR}/opendictate"
echo "Ярлык:     ${APP_DIR}/opendictate.desktop"
echo ""
echo "Запуск: из системного меню приложений или командой opendictate"
echo ""
