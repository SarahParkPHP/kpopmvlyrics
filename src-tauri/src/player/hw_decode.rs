use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use gstreamer as gst;
use gstreamer::prelude::PluginFeatureExtManual;

#[derive(Debug, Clone)]
pub struct HwDecodeProfile {
    pub nvidia_gpu: Option<String>,
    pub va_gpu: Option<String>,
    pub prefer_nvidia: bool,
    pub nvdec: [bool; 4],
    pub vaapi: [bool; 4],
}

impl HwDecodeProfile {
    fn probe() -> Self {
        let nvidia_gpu = detect_nvidia_gpu();
        let va_gpu = detect_va_gpu();
        let prefer_nvidia = nvidia_gpu.is_some() && nvdec_any_available();

        let nvdec = [
            element_exists("nvh264dec"),
            element_exists("nvh265dec"),
            element_exists("nvvp9dec"),
            element_exists("nvav1dec"),
        ];
        let vaapi = [
            element_exists("vah264dec") || element_exists("vaapih264dec"),
            element_exists("vah265dec") || element_exists("vaapih265dec"),
            element_exists("vavp9dec") || element_exists("vaapivp9dec"),
            element_exists("vaav1dec"),
        ];

        Self {
            nvidia_gpu,
            va_gpu,
            prefer_nvidia,
            nvdec,
            vaapi,
        }
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(name) = &self.nvidia_gpu {
            parts.push(format!("NVIDIA {name}"));
        }
        if let Some(name) = &self.va_gpu {
            parts.push(format!("VA-API {name}"));
        }
        if parts.is_empty() {
            parts.push("software decode".to_string());
        }
        if self.prefer_nvidia {
            parts.push("prefer NVDEC".to_string());
        }
        parts.join(", ")
    }
}

static PROFILE: OnceLock<HwDecodeProfile> = OnceLock::new();

pub fn hw_decode_profile() -> &'static HwDecodeProfile {
    PROFILE.get_or_init(HwDecodeProfile::probe)
}

pub fn prepare_environment() {
    if Path::new("/dev/nvidia0").exists() {
        prime_nvidia_environment();
    }
}

pub fn configure_decoder_ranks() {
    let profile = hw_decode_profile();
    eprintln!("kpopmvlyrics: hardware decode profile: {}", profile.summary());

    let hw_rank = gst::Rank::PRIMARY + 500;
    let fallback_rank = gst::Rank::SECONDARY;
    let software_rank = gst::Rank::MARGINAL;

    let nv_decoders = [
        ("nvh264dec", profile.nvdec[0]),
        ("nvh265dec", profile.nvdec[1]),
        ("nvvp9dec", profile.nvdec[2]),
        ("nvav1dec", profile.nvdec[3]),
    ];
    let va_decoders = [
        ("vah264dec", profile.vaapi[0]),
        ("vah265dec", profile.vaapi[1]),
        ("vavp9dec", profile.vaapi[2]),
        ("vaav1dec", profile.vaapi[3]),
        ("vaapih264dec", profile.vaapi[0]),
        ("vaapih265dec", profile.vaapi[1]),
        ("vaapivp9dec", profile.vaapi[2]),
    ];
    let software_decoders = [
        "avdec_h264",
        "avdec_h265",
        "avdec_hevc",
        "avdec_vp8",
        "avdec_vp9",
        "avdec_av1",
    ];

    if profile.prefer_nvidia {
        for (name, available) in nv_decoders {
            set_rank(name, if available { hw_rank } else { fallback_rank });
        }
        for (name, available) in va_decoders {
            let rank = if available {
                fallback_rank
            } else {
                gst::Rank::NONE
            };
            set_rank(name, rank);
        }
    } else {
        for (name, available) in va_decoders {
            set_rank(name, if available { hw_rank } else { fallback_rank });
        }
        for (name, available) in nv_decoders {
            set_rank(name, if available { fallback_rank } else { gst::Rank::NONE });
        }
    }

    for name in software_decoders {
        set_rank(name, software_rank);
    }

    if profile.prefer_nvidia && element_exists("cudadownload") {
        set_rank("cudadownload", hw_rank);
        set_rank("cudaconvert", hw_rank);
        set_rank("cudaconvertscale", hw_rank);
    }
}

fn set_rank(name: &str, rank: gst::Rank) {
    if let Some(factory) = gst::ElementFactory::find(name) {
        factory.set_rank(rank);
    }
}

fn element_exists(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

fn nvdec_any_available() -> bool {
    ["nvh264dec", "nvh265dec", "nvvp9dec", "nvav1dec"]
        .iter()
        .any(|name| element_exists(name))
}

fn detect_nvidia_gpu() -> Option<String> {
    if !Path::new("/dev/nvidia0").exists() {
        return None;
    }

    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
        .ok()?;
    if !output.status.success() {
        return Some("NVIDIA GPU".to_string());
    }

    let name = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .unwrap_or("NVIDIA GPU")
        .to_string();
    Some(name)
}

fn detect_va_gpu() -> Option<String> {
    let render_nodes: Vec<_> = std::fs::read_dir("/dev/dri")
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD"))
        })
        .collect();

    if render_nodes.is_empty() {
        return None;
    }

    for node in render_nodes {
        let display = format!("drm:{}", node.to_string_lossy());
        if let Ok(output) = Command::new("vainfo").arg("--display").arg(&display).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = text.lines().find(|line| line.contains("Driver version")) {
                return Some(line.trim().to_string());
            }
        }
    }

    Some("VA-API device".to_string())
}

fn prime_nvidia_environment() {
    std::env::set_var("__NV_PRIME_RENDER_OFFLOAD", "1");
    std::env::set_var("__GLX_VENDOR_LIBRARY_NAME", "nvidia");
    std::env::set_var("CUDA_VISIBLE_DEVICES", "0");
    std::env::set_var("GST_CUDA_DEVICE_ID", "0");
}

pub fn caps_use_cuda_memory(caps: &gst::Caps) -> bool {
    caps.to_string().contains("CUDAMemory")
}
