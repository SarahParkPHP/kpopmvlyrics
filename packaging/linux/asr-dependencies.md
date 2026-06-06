# ASR and STT Dependencies

K-Pop MV Lyrics can align from YouTube captions without ASR. Speech-to-text is
optional and is enabled from the app settings.

Required runtime packages for the native Linux app:

- GTK 4
- GDK Pixbuf image loading
- GStreamer runtime and common playback plugins
- `yt-dlp`
- CA certificates

Optional packages for any ASR/STT backend:

- `python3`
- `ffmpeg`

Optional packages for local Qwen ASR and Demucs vocal separation:

- `python3-venv`
- `python3-pip`
- PyTorch, preferably with CUDA on GPU systems

Install the Python ASR environment from the project checkout with:

```bash
./scripts/setup-asr.sh
```

That environment installs the Python packages from `requirements-asr.txt`,
including `qwen-asr` and `demucs`.

External STT providers do not need local ASR model weights, but they still use
the bundled Python bridge. Enter the API key in the app settings after choosing
OpenAI, ElevenLabs Scribe v2, Mistral, Gemini, Soniox, or Alibaba
Qwen3-ASR-Flash from the ASR model dropdown.

Demucs is optional. When enabled, the app runs Demucs `htdemucs` in two-stem
vocals mode before sending audio to the selected ASR/STT model.
