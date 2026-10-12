// Звуковой синтезатор коротких системных сигналов (бипов) для обратной связи без внешних файлов.
// Генерирует чистый синусоидальный тон и воспроизводит через parec/paplay в PipeWire.

use std::io::Write;
use std::process::{Command, Stdio};

pub fn play_start_sound() {
    play_tone(880.0, 70); // Высокий короткий бип (880 Гц, 70 мс) — старт записи
}

pub fn play_stop_sound() {
    play_tone(440.0, 80); // Низкий короткий бип (440 Гц, 80 мс) — окончание записи
}

fn play_tone(freq: f32, duration_ms: u32) {
    std::thread::spawn(move || {
        let sample_rate = 44100;
        let num_samples = (sample_rate as f32 * (duration_ms as f32 / 1000.0)) as usize;
        let mut pcm = Vec::with_capacity(num_samples * 2);

        for i in 0..num_samples {
            let t = i as f32 / sample_rate as f32;
            // Мягкое огибание по краям, чтобы избежать щелчка (fade in / fade out)
            let envelope = if i < 150 {
                i as f32 / 150.0
            } else if i > num_samples - 150 {
                (num_samples - i) as f32 / 150.0
            } else {
                1.0
            };
            let sample = (f32::sin(2.0 * std::f32::consts::PI * freq * t) * 12000.0 * envelope) as i16;
            pcm.extend_from_slice(&sample.to_le_bytes());
        }

        if let Ok(mut child) = Command::new("paplay")
            .args(["--raw", "--format=s16le", "--rate=44100", "--channels=1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&pcm);
            }
            let _ = child.wait();
        }
    });
}
