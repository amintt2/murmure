# Murmure

Dictée locale, hors-ligne, par raccourci clavier. macOS et Windows.

- Moteurs : llama.cpp (Qwen3-ASR, Voxtral), whisper.cpp (Whisper), sherpa-onnx (Parakeet, Canary).
- Raccourci global → overlay en bas d'écran → texte inséré dans le champ actif ou copié.

## Développement

```bash
npm install
npx tauri dev
```

macOS : `brew install llama.cpp whisper-cpp` (sherpa-onnx est téléchargé automatiquement).
Windows : les runtimes sont embarqués dans l'installeur (voir `.github/workflows/windows.yml`).
