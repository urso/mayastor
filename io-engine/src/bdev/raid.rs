use std::{
    collections::HashMap,
    convert::TryFrom,
    fmt::{Debug, Formatter},
};

use async_trait::async_trait;
use snafu::ResultExt;
use url::Url;

use spdk_rs::libspdk;

use crate::{
    bdev::{dev::reject_unknown_parameters, util::uri, CreateDestroy, GetName},
    bdev_api::{self, BdevError},
    core::{raid::RaidBdev, UntypedBdev},
};

#[derive(Debug)]
pub struct RaidLevel {
    name: &'static str,
    spdk_raid_level: i32,
    min_devices: usize,
    default_strip_size_kb: u32,
    strip_size_required: bool,
}

impl RaidLevel {
    fn validate(&self, children_count: usize, strip_size_kb: u32) -> Result<(), String> {
        if children_count < self.min_devices {
            return Err(format!("Needs at least {} devices", self.min_devices));
        }

        if self.strip_size_required {
            if !(4..=65536).contains(&strip_size_kb) {
                return Err(format!(
                    "Strip size {strip_size_kb}KB out of range [4, 65536]"
                ));
            }

            if !strip_size_kb.is_power_of_two() {
                return Err(format!("Strip size {strip_size_kb}KiB must be power of 2"));
            }
        }

        Ok(())
    }
}

pub const RAID0: &RaidLevel = &RaidLevel {
    name: "RAID0",
    spdk_raid_level: libspdk::RAID0,
    min_devices: 2,
    default_strip_size_kb: 64,
    strip_size_required: true,
};

pub const RAID1: &RaidLevel = &RaidLevel {
    name: "RAID1",
    spdk_raid_level: libspdk::RAID1,
    min_devices: 2,
    default_strip_size_kb: 0,
    strip_size_required: false,
};

pub const RAID5F: &RaidLevel = &RaidLevel {
    name: "RAID5F",
    spdk_raid_level: libspdk::RAID5F,
    min_devices: 3,
    default_strip_size_kb: 64,
    strip_size_required: true,
};

pub const CONCAT: &RaidLevel = &RaidLevel {
    name: "CONCAT",
    spdk_raid_level: libspdk::CONCAT,
    min_devices: 1,
    default_strip_size_kb: 0,
    strip_size_required: false,
};

/// Generic RAID bdev configuration and management structure
pub struct Raid {
    /// the name of the bdev we created, this is equal to the URI path minus
    /// the leading '/'
    name: String,
    /// alias which can be used to open the bdev
    alias: String,
    /// the list of child devices.
    disks: Vec<String>,
    /// the devices will be striped across in chunks of this size in KB (must be power of 2, >= 4KB, <= 64MB)
    /// For levels that don't use strip_size, this field is ignored
    strip_size_kb: u32,
    /// uuid for the spdk bdev
    uuid: uuid::Uuid,
    /// Marker for the RAID level
    level: &'static RaidLevel,
}

impl Debug for Raid {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Raid{} '{}' (strip_size: {}KB) <= {:?}",
            self.level.name, self.name, self.strip_size_kb, self.disks
        )
    }
}

impl TryFrom<&Url> for Raid {
    type Error = BdevError;

    fn try_from(uri: &Url) -> Result<Self, Self::Error> {
        let scheme = uri.scheme();
        let level = match scheme {
            "raid0" => &RAID0,
            "raid1" => &RAID1,
            "raid5f" => &RAID5F,
            "concat" => &CONCAT,
            _ => {
                return Err(BdevError::InvalidUri {
                    uri: uri.to_string(),
                    message: "invalid scheme".to_string(),
                })
            }
        };

        let segments = uri::segments(uri);
        if segments.is_empty() {
            return Err(BdevError::InvalidUri {
                uri: uri.to_string(),
                message: "empty path".to_string(),
            });
        }

        let mut parameters: HashMap<String, String> = uri.query_pairs().into_owned().collect();

        // Parse strip size - use default if not provided and level requires it
        let strip_size_kb: u32 = if level.strip_size_required {
            parameters
                .remove("strip_size")
                .map_or(Ok(level.default_strip_size_kb), |value| {
                    value.parse().context(bdev_api::IntParamParseFailed {
                        uri: uri.to_string(),
                        parameter: String::from("strip_size"),
                        value: value.clone(),
                    })
                })?
        } else {
            // For levels that don't require strip_size, ignore it if provided
            parameters.remove("strip_size");
            level.default_strip_size_kb
        };

        let uuid = match parameters.remove("uuid") {
            Some(uuid_str) => {
                // UUID parameter provided - parse it, fail if invalid
                uri::uuid(Some(uuid_str))
                    .context(bdev_api::UuidParamParseFailed {
                        uri: uri.to_string(),
                    })?
                    .expect("uri::uuid should return Some when given Some input")
            }
            None => {
                // No UUID parameter provided - generate random one
                uuid::Uuid::new_v4()
            }
        };

        let children: Vec<String> = parameters
            .remove("children")
            .ok_or_else(|| BdevError::InvalidUri {
                uri: uri.to_string(),
                message: "'children' must be specified".to_string(),
            })?
            .split(',')
            .map(|s| s.to_string())
            .collect();

        // Level-specific validation
        level
            .validate(children.len(), strip_size_kb)
            .map_err(|e| BdevError::InvalidUri {
                uri: uri.to_string(),
                message: e,
            })?;

        reject_unknown_parameters(uri, parameters)?;

        let name = uri.path()[1..].into();
        let alias = uri.to_string();
        Ok(Self::new(name, alias, children, level, uuid, strip_size_kb))
    }
}

impl GetName for Raid {
    fn get_name(&self) -> String {
        self.name.clone()
    }
}

impl super::Probe for Raid {}

#[async_trait(?Send)]
impl CreateDestroy for Raid {
    type Error = BdevError;

    async fn create(&self) -> Result<String, Self::Error> {
        debug!("{:?}: creating {} bdev", self, self.level.name);

        if UntypedBdev::lookup_by_name(&self.name).is_some() {
            return Err(BdevError::BdevExists {
                name: self.name.clone(),
            });
        }

        for child_name in &self.disks {
            debug!("{}: looking up child device: '{}'", self.name, child_name);
            if UntypedBdev::lookup_by_name(child_name).is_none() {
                error!("{}: child device '{}' not found", self.name, child_name);
                return Err(BdevError::CreateBdevFailedStr {
                    error: format!("Child device '{child_name}' not found"),
                    name: self.name.clone(),
                });
            }
        }

        self.create_raid_bdev(&self.disks).await
    }

    async fn destroy(self: Box<Self>) -> Result<(), Self::Error> {
        debug!("{}: destroying {} bdev", self.name, self.level.name);
        if let Some(raid_bdev) = RaidBdev::find_by_name(&self.name) {
            raid_bdev.delete().await?;
        } else {
            return Err(BdevError::BdevNotFound {
                name: self.name.clone(),
            });
        }
        Ok(())
    }
}

impl Raid {
    pub fn new(
        name: String,
        alias: String,
        disks: Vec<String>,
        level: &'static RaidLevel,
        uuid: uuid::Uuid,
        strip_size_kb: u32,
    ) -> Self {
        Self {
            name,
            alias,
            disks,
            level,
            uuid,
            strip_size_kb,
        }
    }

    /// Create the RAID bdev using SPDK API - now generic across all RAID levels
    async fn create_raid_bdev(&self, device_names: &[String]) -> Result<String, BdevError> {
        let mut raid_bdev = RaidBdev::create(
            &self.name,
            Some(self.uuid),
            self.strip_size_kb,
            device_names.len() as u8,
            self.level.spdk_raid_level,
        )?;

        if let Err(e) = self
            .init_raid_bdev_devices(&mut raid_bdev, device_names)
            .await
        {
            error!(
                "{}: failed to initialize RAID bdev devices: {}",
                self.name, e
            );
            if let Err(e) = raid_bdev.delete().await {
                warn!("{}: failed to delete RAID bdev: {}", self.name, e);
            }
            return Err(e);
        }

        if let Some(mut bdev) = UntypedBdev::lookup_by_name(&self.name) {
            if self.alias != self.name && !bdev.add_alias(&self.alias) {
                warn!("{}: failed to add alias '{}'", self.name, self.alias);
            }
        }

        debug!(
            "{}: RAID{} bdev creation complete",
            self.name, self.level.spdk_raid_level
        );
        Ok(self.name.clone())
    }

    async fn init_raid_bdev_devices(
        &self,
        raid_bdev: &mut RaidBdev,
        device_names: &[String],
    ) -> Result<(), BdevError> {
        for device_name in device_names {
            if let Err(e) = raid_bdev.add_base_bdev(device_name).await {
                error!(
                    "{}: failed to add base device '{}': {}",
                    self.name, device_name, e
                );
                return Err(e);
            }
        }

        if let Err(e) = raid_bdev.wait_for_online(None).await {
            error!(
                "{}: failed to wait for RAID bdev to be online: {}",
                self.name, e
            );
            return Err(e.into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uri_parsing_success() {
        // RAID0 with custom strip size
        let uri =
            Url::parse("raid0:///my_raid?strip_size=128&children=aio:///dev/sda,malloc:///mem1")
                .unwrap();
        let raid = Raid::try_from(&uri).unwrap();
        assert_eq!(raid.name, "my_raid");
        assert_eq!(raid.strip_size_kb, 128);
        assert_eq!(raid.disks, vec!["aio:///dev/sda", "malloc:///mem1"]);
        assert_eq!(raid.level.name, "RAID0");

        // RAID1 (strip size ignored)
        let uri = Url::parse("raid1:///mirror?children=dev1,dev2").unwrap();
        let raid = Raid::try_from(&uri).unwrap();
        assert_eq!(raid.name, "mirror");
        assert_eq!(raid.strip_size_kb, 0);
        assert_eq!(raid.level.name, "RAID1");

        // RAID5F with default strip size
        let uri = Url::parse("raid5f:///parity?children=dev1,dev2,dev3").unwrap();
        let raid = Raid::try_from(&uri).unwrap();
        assert_eq!(raid.strip_size_kb, 64);
        assert_eq!(raid.level.name, "RAID5F");

        // CONCAT
        let uri = Url::parse("concat:///linear?children=dev1").unwrap();
        let raid = Raid::try_from(&uri).unwrap();
        assert_eq!(raid.level.name, "CONCAT");
    }

    #[test]
    fn test_uri_parsing_failures() {
        // Invalid scheme
        let uri = Url::parse("invalid:///test?children=dev1,dev2").unwrap();
        assert!(Raid::try_from(&uri).is_err());

        // Empty path
        let uri = Url::parse("raid0:///?children=dev1,dev2").unwrap();
        assert!(Raid::try_from(&uri).is_err());

        // Missing children parameter
        let uri = Url::parse("raid0:///test").unwrap();
        assert!(Raid::try_from(&uri).is_err());
    }

    #[test]
    fn test_validation_failures() {
        // RAID0: insufficient children
        let uri = Url::parse("raid0:///test?children=dev1").unwrap();
        assert!(Raid::try_from(&uri).is_err());

        // RAID0: invalid strip size (not power of 2)
        let uri = Url::parse("raid0:///test?strip_size=63&children=dev1,dev2").unwrap();
        assert!(Raid::try_from(&uri).is_err());

        // RAID0: strip size out of range
        let uri = Url::parse("raid0:///test?strip_size=131072&children=dev1,dev2").unwrap();
        assert!(Raid::try_from(&uri).is_err());

        // RAID5F: insufficient children (needs at least 3)
        let uri = Url::parse("raid5f:///test?children=dev1,dev2").unwrap();
        assert!(Raid::try_from(&uri).is_err());

        // RAID1: insufficient children (needs at least 2)
        let uri = Url::parse("raid1:///test?children=dev1").unwrap();
        assert!(Raid::try_from(&uri).is_err());
    }
}
