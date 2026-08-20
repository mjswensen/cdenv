//! Reliable/unknown host capability evaluation for Dev Container requirements.

use serde::Serialize;
use thiserror::Error;

use crate::{GpuRequirement, HostRequirements};

/// A measured value or an explicit reason it cannot be measured reliably.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measured<T> {
    /// Reliable local Docker/host evidence.
    Reliable(T),
    /// No reliable evidence is available.
    Unknown(UnknownMeasurement),
}

/// Stable reason a host capability is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UnknownMeasurement {
    /// The platform has no reliable probe for this value.
    Unmeasurable,
    /// A normally reliable probe was unavailable.
    ProbeUnavailable,
}

/// Injected GPU capabilities. `None` means reliable evidence that no GPU is available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuCapabilities {
    /// Reliably measured GPU cores, when the provider exposes them.
    pub cores: Option<u64>,
    /// Reliably measured GPU memory in bytes, when exposed.
    pub memory_bytes: Option<u64>,
}

/// Injected host/Docker capabilities. Evaluation performs no probing itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostCapabilities {
    /// Available logical CPUs.
    pub cpus: Measured<u64>,
    /// Available memory in bytes.
    pub memory_bytes: Measured<u64>,
    /// Available storage in bytes.
    pub storage_bytes: Measured<u64>,
    /// Available GPU or reliable absence. Unknown means GPU presence cannot be proved.
    pub gpu: Measured<Option<GpuCapabilities>>,
}

/// Whether the eventual create plan may request GPU access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GpuAccessIntent {
    /// Configuration did not request GPU access.
    None,
    /// Configuration explicitly requested required, optional, or detailed GPU access.
    Requested,
}

/// Resource named by a host requirement diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HostResource {
    /// Logical processors.
    Cpu,
    /// Memory.
    Memory,
    /// Storage.
    Storage,
    /// GPU presence.
    Gpu,
    /// GPU cores.
    GpuCores,
    /// GPU memory.
    GpuMemory,
}

impl HostResource {
    const fn property_path(self) -> &'static str {
        match self {
            Self::Cpu => "$.hostRequirements.cpus",
            Self::Memory => "$.hostRequirements.memory",
            Self::Storage => "$.hostRequirements.storage",
            Self::Gpu => "$.hostRequirements.gpu",
            Self::GpuCores => "$.hostRequirements.gpu.cores",
            Self::GpuMemory => "$.hostRequirements.gpu.memory",
        }
    }
}

/// Non-fatal host requirement finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HostRequirementWarning {
    /// Resource that could not satisfy a definitive check.
    pub resource: HostResource,
    /// Why this finding is only a warning.
    pub kind: HostRequirementWarningKind,
}

/// Stable warning category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HostRequirementWarningKind {
    /// No reliable measurement exists.
    Unknown(UnknownMeasurement),
    /// Optional GPU access is unavailable.
    OptionalGpuUnavailable,
    /// Optional GPU presence cannot be measured.
    OptionalGpuUnknown(UnknownMeasurement),
    /// GPU exists but detailed core/memory data is not exposed.
    GpuDetailUnknown,
}

/// Successful evaluation. Absence of warnings means all requirements were proved met.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostRequirementEvaluation {
    /// Non-fatal findings.
    pub warnings: Vec<HostRequirementWarning>,
    /// Explicit GPU grant intent for later create planning.
    pub gpu_access: GpuAccessIntent,
}

impl HostRequirementEvaluation {
    /// Reports whether every configured requirement was reliably proved met.
    #[must_use]
    pub fn is_proved_met(&self) -> bool {
        self.warnings.is_empty()
    }
}

/// Reliable evidence that a hard host requirement is unmet.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{property_path}: reliable host capability evidence proves the requirement is unmet")]
pub struct HostRequirementError {
    /// Exact requirement property path.
    pub property_path: &'static str,
    /// Unmet resource; measured and required values are intentionally omitted.
    pub resource: HostResource,
}

/// Evaluates merged host requirements against injected measured/unknown capabilities.
///
/// Unknown CPU, memory, storage, and GPU values produce warnings. Only reliable evidence can
/// return an error. Optional GPU absence always warns. GPU access is never requested unless the
/// configuration explicitly contains a positive, optional, or detailed GPU requirement.
///
/// # Errors
///
/// Returns the first property-ordered hard requirement reliably proved unmet, or an overflow-safe
/// byte requirement error at the corresponding property path.
pub fn evaluate_host_requirements(
    requirements: Option<&HostRequirements>,
    capabilities: &HostCapabilities,
) -> Result<HostRequirementEvaluation, HostRequirementError> {
    let Some(requirements) = requirements else {
        return Ok(HostRequirementEvaluation {
            warnings: Vec::new(),
            gpu_access: GpuAccessIntent::None,
        });
    };
    let mut warnings = Vec::new();
    if let Some(required) = requirements.cpus {
        evaluate_scalar(
            required,
            capabilities.cpus,
            HostResource::Cpu,
            &mut warnings,
        )?;
    }
    if let Some(required) = requirements.memory.as_deref() {
        let required = byte_quantity(required).ok_or(HostRequirementError {
            property_path: HostResource::Memory.property_path(),
            resource: HostResource::Memory,
        })?;
        evaluate_scalar(
            required,
            capabilities.memory_bytes,
            HostResource::Memory,
            &mut warnings,
        )?;
    }
    if let Some(required) = requirements.storage.as_deref() {
        let required = byte_quantity(required).ok_or(HostRequirementError {
            property_path: HostResource::Storage.property_path(),
            resource: HostResource::Storage,
        })?;
        evaluate_scalar(
            required,
            capabilities.storage_bytes,
            HostResource::Storage,
            &mut warnings,
        )?;
    }
    let gpu_access = evaluate_gpu(requirements.gpu.as_ref(), capabilities.gpu, &mut warnings)?;
    Ok(HostRequirementEvaluation {
        warnings,
        gpu_access,
    })
}

fn evaluate_scalar(
    required: u64,
    measured: Measured<u64>,
    resource: HostResource,
    warnings: &mut Vec<HostRequirementWarning>,
) -> Result<(), HostRequirementError> {
    match measured {
        Measured::Reliable(available) if available < required => Err(HostRequirementError {
            property_path: resource.property_path(),
            resource,
        }),
        Measured::Reliable(_) => Ok(()),
        Measured::Unknown(reason) => {
            warnings.push(HostRequirementWarning {
                resource,
                kind: HostRequirementWarningKind::Unknown(reason),
            });
            Ok(())
        }
    }
}

fn evaluate_gpu(
    requirement: Option<&GpuRequirement>,
    measured: Measured<Option<GpuCapabilities>>,
    warnings: &mut Vec<HostRequirementWarning>,
) -> Result<GpuAccessIntent, HostRequirementError> {
    let Some(requirement) = requirement else {
        return Ok(GpuAccessIntent::None);
    };
    match requirement {
        GpuRequirement::NotRequired => Ok(GpuAccessIntent::None),
        GpuRequirement::Optional => match measured {
            Measured::Reliable(Some(_)) => Ok(GpuAccessIntent::Requested),
            Measured::Reliable(None) => {
                warnings.push(HostRequirementWarning {
                    resource: HostResource::Gpu,
                    kind: HostRequirementWarningKind::OptionalGpuUnavailable,
                });
                Ok(GpuAccessIntent::None)
            }
            Measured::Unknown(reason) => {
                warnings.push(HostRequirementWarning {
                    resource: HostResource::Gpu,
                    kind: HostRequirementWarningKind::OptionalGpuUnknown(reason),
                });
                Ok(GpuAccessIntent::None)
            }
        },
        GpuRequirement::Required => {
            require_gpu(measured, warnings)?;
            Ok(GpuAccessIntent::Requested)
        }
        GpuRequirement::Detailed { cores, memory } => {
            let gpu = require_gpu(measured, warnings)?;
            if let Some(required) = cores {
                match gpu.and_then(|value| value.cores) {
                    Some(available) if available < *required => {
                        return Err(HostRequirementError {
                            property_path: HostResource::GpuCores.property_path(),
                            resource: HostResource::GpuCores,
                        });
                    }
                    Some(_) => {}
                    None => warnings.push(HostRequirementWarning {
                        resource: HostResource::GpuCores,
                        kind: HostRequirementWarningKind::GpuDetailUnknown,
                    }),
                }
            }
            if let Some(required) = memory.as_deref() {
                let required = byte_quantity(required).ok_or(HostRequirementError {
                    property_path: HostResource::GpuMemory.property_path(),
                    resource: HostResource::GpuMemory,
                })?;
                match gpu.and_then(|value| value.memory_bytes) {
                    Some(available) if available < required => {
                        return Err(HostRequirementError {
                            property_path: HostResource::GpuMemory.property_path(),
                            resource: HostResource::GpuMemory,
                        });
                    }
                    Some(_) => {}
                    None => warnings.push(HostRequirementWarning {
                        resource: HostResource::GpuMemory,
                        kind: HostRequirementWarningKind::GpuDetailUnknown,
                    }),
                }
            }
            Ok(GpuAccessIntent::Requested)
        }
    }
}

fn require_gpu(
    measured: Measured<Option<GpuCapabilities>>,
    warnings: &mut Vec<HostRequirementWarning>,
) -> Result<Option<GpuCapabilities>, HostRequirementError> {
    match measured {
        Measured::Reliable(None) => Err(HostRequirementError {
            property_path: HostResource::Gpu.property_path(),
            resource: HostResource::Gpu,
        }),
        Measured::Reliable(gpu) => Ok(gpu),
        Measured::Unknown(reason) => {
            warnings.push(HostRequirementWarning {
                resource: HostResource::Gpu,
                kind: HostRequirementWarningKind::Unknown(reason),
            });
            Ok(None)
        }
    }
}

fn byte_quantity(value: &str) -> Option<u64> {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    let number = value.get(..digits)?.parse::<u64>().ok()?;
    let multiplier = match value.get(digits..)? {
        "" => 1,
        "kb" => 1_u64 << 10,
        "mb" => 1_u64 << 20,
        "gb" => 1_u64 << 30,
        "tb" => 1_u64 << 40,
        _ => return None,
    };
    number.checked_mul(multiplier)
}
