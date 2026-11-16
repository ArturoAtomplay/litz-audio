const spanish = document.documentElement.lang === "es";
const words = spanish
    ? {
          lives: "+30 vidas",
          sudo: "visitante no está en el archivo sudoers. Este incidente será reportado.",
          vim: "E37: No se ha escrito desde el último cambio (añade ! para forzar)",
          developer: "¡Ahora eres desarrollador!",
          steps: (left) => `Estás a ${left} pasos de ser desarrollador.`,
      }
    : {
          lives: "+30 lives",
          sudo: "visitor is not in the sudoers file. This incident will be reported.",
          vim: "E37: No write since last change (add ! to override)",
          developer: "You are now a developer!",
          steps: (left) => `You are now ${left} steps away from being a developer.`,
      };

for (const button of document.querySelectorAll(".command button")) {
    const label = button.textContent;
    button.addEventListener("click", async () => {
        await navigator.clipboard.writeText(button.previousElementSibling.textContent);
        button.textContent = button.dataset.done;
        setTimeout(() => (button.textContent = label), 1500);
    });
}

const toast = document.createElement("p");
toast.className = "toast";
toast.setAttribute("role", "status");
document.body.append(toast);
let hide;
function say(text) {
    toast.textContent = text;
    toast.classList.add("shown");
    clearTimeout(hide);
    hide = setTimeout(() => toast.classList.remove("shown"), 3500);
}

const N = { A2: 110, C3: 130.81, F2: 87.31, G2: 98, A4: 440, B4: 493.88, C5: 523.25, D5: 587.33, E5: 659.25, F5: 698.46, G5: 783.99, A5: 880, B5: 987.77, C6: 1046.5, D6: 1174.66, E6: 1318.51, G6: 1567.98 };
const song = [
    { bass: "C3", chord: ["C5", "E5", "G5"], lead: "E5 G5 C6 G5 E5 G5 D5 E5" },
    { bass: "G2", chord: ["B4", "D5", "G5"], lead: "D5 G5 B5 G5 D5 - B4 D5" },
    { bass: "A2", chord: ["C5", "E5", "A5"], lead: "C5 E5 A5 E5 C5 E5 B4 C5" },
    { bass: "F2", chord: ["A4", "C5", "F5"], lead: "A4 C5 F5 - E5 D5 C5 -" },
    { bass: "C3", chord: ["C5", "E5", "G5"], lead: "E5 G5 C6 D6 E6 - C6 G5" },
    { bass: "G2", chord: ["B4", "D5", "G5"], lead: "B5 D6 G6 - D6 B5 G5 -" },
    { bass: "A2", chord: ["C5", "E5", "A5"], lead: "A5 C6 E6 C6 A5 - G5 E5" },
    { bass: "F2", chord: ["A4", "C5", "F5"], lead: "F5 A5 C6 A5 G5 B5 D6 -" },
];
const beat = 60 / 108 / 2;
let music;

function note(type, frequency, start, length, level) {
    const oscillator = music.audio.createOscillator();
    const gain = music.audio.createGain();
    oscillator.type = type;
    oscillator.frequency.value = frequency;
    gain.gain.setValueAtTime(0, start);
    gain.gain.linearRampToValueAtTime(level, start + 0.015);
    gain.gain.exponentialRampToValueAtTime(level * 0.4, start + length * 0.6);
    gain.gain.linearRampToValueAtTime(0, start + length);
    oscillator.connect(gain).connect(music.out);
    oscillator.start(start);
    oscillator.stop(start + length);
}

function loop(start) {
    song.forEach((bar, index) => {
        const at = start + index * 8 * beat;
        note("sine", N[bar.bass], at, 4 * beat, 0.22);
        note("sine", N[bar.bass], at + 4 * beat, 4 * beat, 0.22);
        for (let step = 0; step < 16; step++) {
            note("sine", N[bar.chord[step % 3]] / 2, at + (step * beat) / 2, beat / 2, 0.05);
        }
        bar.lead.split(" ").forEach((name, step, lead) => {
            if (name === "-") return;
            const held = lead[step + 1] === "-" ? 2 : 1;
            note("triangle", N[name], at + step * beat, held * beat * 0.95, 0.16);
        });
    });
    const next = start + song.length * 8 * beat;
    music.timer = setTimeout(() => loop(next), (next - music.audio.currentTime - 1) * 1000);
}

function play() {
    if (music) {
        clearTimeout(music.timer);
        music.audio.close();
        music = undefined;
        document.body.classList.remove("loud");
        return;
    }
    const audio = new AudioContext();
    const out = audio.createGain();
    const echo = audio.createDelay();
    const feedback = audio.createGain();
    const wet = audio.createGain();
    out.gain.value = 0.6;
    echo.delayTime.value = beat * 1.5;
    feedback.gain.value = 0.3;
    wet.gain.value = 0.25;
    out.connect(audio.destination);
    out.connect(echo).connect(feedback).connect(echo);
    echo.connect(wet).connect(audio.destination);
    music = { audio, out };
    loop(audio.currentTime + 0.1);
    say(words.lives);
    document.body.classList.add("loud");
}

const typed = [
    {
        keys: "ArrowUp ArrowUp ArrowDown ArrowDown ArrowLeft ArrowRight ArrowLeft ArrowRight b a".split(" "),
        run: play,
    },
    { keys: [..."sudo"], run: () => say(words.sudo) },
    { keys: [...":q"], run: () => say(words.vim) },
];
const recent = [];
document.addEventListener("keydown", (event) => {
    recent.push(event.key);
    if (recent.length > 10) recent.shift();
    for (const { keys, run } of typed) {
        if (keys.every((key, index) => recent[recent.length - keys.length + index] === key)) {
            recent.length = 0;
            run();
        }
    }
});

let taps = 0;
document.querySelector(".android")?.addEventListener("click", () => {
    taps += 1;
    if (taps === 7) say(words.developer);
    else if (taps > 3 && taps < 7) say(words.steps(7 - taps));
});

console.log("Reading the source? All of it is at https://github.com/ArturoAtomplay/litz-audio");
