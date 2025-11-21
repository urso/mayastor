use common::MayastorTest;
use io_engine::{
    bdev::crypto::{Cipher, EncryptionKey},
    bdev_api::bdev_create,
    core::{logical_volume::LogicalVolume, MayastorCliArgs, Protocol, Share, UntypedBdev},
    lvs::{Lvs, LvsLvol, PropName, PropValue},
    pool_backend::{PoolArgs, PoolBackend, Raid0Config, RaidConfig},
    subsys::NvmfSubsystem,
};
use std::pin::Pin;

pub mod common;

static TESTDIR: &str = "/tmp/io-engine-tests";
static DISKNAME1: &str = "/tmp/io-engine-tests/disk1.img";
static DISKNAME2: &str = "/tmp/io-engine-tests/disk2.img";
static DISKNAME3: &str = "/tmp/io-engine-tests/disk3.img";
static DISK_CRYPTO: &str = "/tmp/io-engine-tests/crypto_disk.img";
static RAID0_DISK1: &str = "/tmp/io-engine-tests/raid0_persist_disk1.img";
static RAID0_DISK2: &str = "/tmp/io-engine-tests/raid0_persist_disk2.img";
static XTS_KEY: &str = "2b7e151628aed2a6abf7158809cf4f3c";
static XTS_KEY2: &str = "2b7e151628aed2a6abf7158809cf4f3d";

#[tokio::test]
async fn lvs_pool_test() {
    // Create directory for placing test disk files
    // todo: Create this from some common place and use for all other tests too.
    let _ = std::process::Command::new("mkdir")
        .args(["-p"])
        .args([TESTDIR])
        .output()
        .expect("failed to execute mkdir");

    common::delete_file(&[
        DISKNAME1.into(),
        DISKNAME2.into(),
        DISKNAME3.into(),
        DISK_CRYPTO.into(),
        RAID0_DISK1.into(),
        RAID0_DISK2.into(),
    ]);
    common::truncate_file(DISKNAME1, 128 * 1024);
    common::truncate_file(DISKNAME2, 128 * 1024);
    common::truncate_file(DISKNAME3, 128 * 1024);
    common::truncate_file(DISK_CRYPTO, 128 * 1024);
    common::truncate_file(RAID0_DISK1, 64 * 1024);
    common::truncate_file(RAID0_DISK2, 64 * 1024);

    //setup disk3 via loop device using a sector size of 4096.
    let ldev = common::setup_loopdev_file(DISKNAME3, Some(4096));

    let args = MayastorCliArgs {
        reactor_mask: "0x3".into(),
        ..Default::default()
    };
    let ms = MayastorTest::new(args);

    // should fail to import a pool that does not exist on disk
    ms.spawn(async {
        assert!(Lvs::import("tpool", format!("aio://{DISKNAME1}").as_str())
            .await
            .is_err())
    })
    .await;

    let pool_args = PoolArgs {
        name: "tpool".into(),
        disks: vec![format!("aio://{DISKNAME1}")],
        uuid: None,
        cluster_size: None,
        md_args: None,
        backend: PoolBackend::Lvs,
        enc_key: None,
        crypto_vbdev_name: None,
        raid_config: None,
    };

    // should succeed to create a pool we can not import
    ms.spawn({
        let pool_args = pool_args.clone();
        async {
            Lvs::create_or_import(pool_args).await.unwrap();
        }
    })
    .await;

    // should fail to create the pool again, notice that we use
    // create directly here to ensure that if we
    // have an idempotent snafu, we dont crash and
    // burn
    ms.spawn(async { assert!(Lvs::create_from_args_inner(pool_args).await.is_err()) })
        .await;

    // should fail to import the pool that is already imported
    // similar to above, we use the import directly
    ms.spawn(async {
        assert!(Lvs::import("tpool", format!("aio://{DISKNAME1}").as_str())
            .await
            .is_err())
    })
    .await;

    // should be able to find our new LVS
    ms.spawn(async {
        assert_eq!(Lvs::iter().count(), 1);
        let pool = Lvs::lookup("tpool").unwrap();
        assert_eq!(pool.name(), "tpool");
        assert_eq!(pool.used(), 0);
        dbg!(pool.uuid());
        assert_eq!(pool.base_bdev().name(), DISKNAME1);
    })
    .await;

    // export the pool keeping the bdev alive and then
    // import the pool and validate the uuid

    ms.spawn(async {
        let pool = Lvs::lookup("tpool").unwrap();
        let uuid = pool.uuid();
        pool.export().await.unwrap();

        // import and export implicitly destroy the base_bdev, for
        // testing import and create we
        // sometimes create the base_bdev manually
        bdev_create(format!("aio://{DISKNAME1}").as_str())
            .await
            .unwrap();

        assert!(Lvs::import("tpool", format!("aio://{DISKNAME1}").as_str())
            .await
            .is_ok());

        let pool = Lvs::lookup("tpool").unwrap();
        assert_eq!(pool.uuid(), uuid);
    })
    .await;

    // destroy the pool, a import should now fail, creating a new
    // pool should not having a matching UUID of the
    // old pool
    ms.spawn(async {
        let pool = Lvs::lookup("tpool").unwrap();
        let uuid = pool.uuid();
        pool.destroy().await.unwrap();

        bdev_create(format!("aio://{DISKNAME1}").as_str())
            .await
            .unwrap();
        assert!(Lvs::import("tpool", format!("aio://{DISKNAME1}").as_str())
            .await
            .is_err());

        assert_eq!(Lvs::iter().count(), 0);
        assert!(Lvs::create_from_args_inner(PoolArgs {
            name: "tpool".to_string(),
            disks: vec![format!("aio://{DISKNAME1}")],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: None,
        })
        .await
        .is_ok());

        let pool = Lvs::lookup("tpool").unwrap();
        assert_ne!(uuid, pool.uuid());
        assert_eq!(Lvs::iter().count(), 1);
    })
    .await;

    // create 10 lvol on this pool
    ms.spawn(async {
        let pool = Lvs::lookup("tpool").unwrap();
        for i in 0..10 {
            pool.create_lvol(&format!("vol-{i}"), 8 * 1024 * 1024, None, true, None)
                .await
                .unwrap();
        }

        assert_eq!(pool.lvols().unwrap().count(), 10);
    })
    .await;

    // create a second pool and ensure it filters correctly
    ms.spawn(async {
        let pool2 = Lvs::create_or_import(PoolArgs {
            name: "tpool2".to_string(),
            disks: vec!["malloc:///malloc0?size_mb=64".to_string()],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: None,
        })
        .await
        .unwrap();

        for i in 0..5 {
            pool2
                .create_lvol(
                    &format!("pool2-vol-{i}"),
                    8 * 1024 * 1024,
                    None,
                    false,
                    None,
                )
                .await
                .unwrap();
        }

        assert_eq!(pool2.lvols().unwrap().count(), 5);

        let pool = Lvs::lookup("tpool").unwrap();
        assert_eq!(pool.lvols().unwrap().count(), 10);
    })
    .await;

    // export the first pool and import it again, all replica's
    // should be present, destroy  all of them by name to
    // ensure they are all there

    ms.spawn(async {
        let pool = Lvs::lookup("tpool").unwrap();
        pool.export().await.unwrap();
        let pool = Lvs::create_or_import(PoolArgs {
            name: "tpool".to_string(),
            disks: vec![format!("aio://{DISKNAME1}")],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: None,
        })
        .await
        .unwrap();

        assert_eq!(pool.lvols().unwrap().count(), 10);

        let df = pool
            .lvols()
            .unwrap()
            .map(|r| r.destroy())
            .collect::<Vec<_>>();
        assert_eq!(df.len(), 10);
        futures::future::join_all(df).await;
    })
    .await;

    // share all the replica's on the pool tpool2
    ms.spawn(async {
        let pool2 = Lvs::lookup("tpool2").unwrap();
        for mut l in pool2.lvols().unwrap() {
            Pin::new(&mut l).share_nvmf(None).await.unwrap();
        }
    })
    .await;

    // destroy the pool and verify that all nvmf shares are removed
    ms.spawn(async {
        let p = Lvs::lookup("tpool2").unwrap();
        p.destroy().await.unwrap();
        assert_eq!(
            NvmfSubsystem::first().unwrap().into_iter().count(),
            1 // only the discovery system remains
        )
    })
    .await;

    // test setting the share property that is stored on disk
    ms.spawn(async {
        let pool = Lvs::lookup("tpool").unwrap();
        let mut lvol = pool
            .create_lvol("vol-1", 1024 * 1024 * 8, None, false, None)
            .await
            .unwrap();

        {
            let mut lvol = Pin::new(&mut lvol);

            lvol.as_mut().set(PropValue::Shared(true)).await.unwrap();
            assert_eq!(
                lvol.get(PropName::Shared).await.unwrap(),
                PropValue::Shared(true)
            );

            lvol.as_mut().set(PropValue::Shared(false)).await.unwrap();
            assert_eq!(
                lvol.get(PropName::Shared).await.unwrap(),
                PropValue::Shared(false)
            );

            // sharing should set the property on disk

            lvol.as_mut().share_nvmf(None).await.unwrap();

            assert_eq!(
                lvol.get(PropName::Shared).await.unwrap(),
                PropValue::Shared(true)
            );

            lvol.as_mut().unshare().await.unwrap();

            assert_eq!(
                lvol.get(PropName::Shared).await.unwrap(),
                PropValue::Shared(false)
            );
        }

        lvol.destroy().await.unwrap();
    })
    .await;

    // create 10 shares, 1 unshared lvol and export the pool
    ms.spawn(async {
        let pool = Lvs::lookup("tpool").unwrap();

        for i in 0..10 {
            pool.create_lvol(&format!("vol-{i}"), 8 * 1024 * 1024, None, true, None)
                .await
                .unwrap();
        }

        for mut l in pool.lvols().unwrap() {
            let l = Pin::new(&mut l);
            l.share_nvmf(None).await.unwrap();
        }

        pool.create_lvol("notshared", 8 * 1024 * 1024, None, true, None)
            .await
            .unwrap();

        pool.export().await.unwrap();
    })
    .await;

    // import the pool all shares should be there, but also validate
    // the share that not shared to be -- not shared
    ms.spawn(async {
        bdev_create(format!("aio://{DISKNAME1}").as_str())
            .await
            .unwrap();
        let pool = Lvs::import("tpool", format!("aio://{DISKNAME1}").as_str())
            .await
            .unwrap();

        for l in pool.lvols().unwrap() {
            if l.name() == "notshared" {
                assert_eq!(l.shared().unwrap(), Protocol::Off);
            } else {
                assert_eq!(l.shared().unwrap(), Protocol::Nvmf);
            }
        }

        assert_eq!(NvmfSubsystem::first().unwrap().into_iter().count(), 1 + 10);
    })
    .await;

    // lastly destroy the pool, import/create it again, no shares
    // should be present
    ms.spawn(async {
        let pool = Lvs::lookup("tpool").unwrap();
        pool.destroy().await.unwrap();
        assert_eq!(NvmfSubsystem::first().unwrap().into_iter().count(), 1);

        let pool = Lvs::create_or_import(PoolArgs {
            name: "tpool".into(),
            disks: vec![format!("aio://{DISKNAME1}")],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: None,
        })
        .await
        .unwrap();

        assert_eq!(NvmfSubsystem::first().unwrap().into_iter().count(), 1);

        assert_eq!(pool.lvols().unwrap().count(), 0);
        pool.export().await.unwrap();
    })
    .await;

    let pool_dev_aio = ldev.clone();
    // should succeed to create an aio bdev pool on a loop blockdev of 4096
    // bytes sector size.
    ms.spawn(async move {
        Lvs::create_or_import(PoolArgs {
            name: "tpool_4k_aio".into(),
            disks: vec![format!("aio://{pool_dev_aio}")],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: None,
        })
        .await
        .unwrap();
    })
    .await;

    // should be able to find our new LVS created on loopdev, and subsequently
    // destroy it.
    ms.spawn(async {
        let pool = Lvs::lookup("tpool_4k_aio").unwrap();
        assert_eq!(pool.name(), "tpool_4k_aio");
        assert_eq!(pool.used(), 0);
        dbg!(pool.uuid());
        pool.destroy().await.unwrap();
    })
    .await;

    let pool_dev_uring = ldev.clone();
    // should succeed to create an uring pool on a loop blockdev of 4096 bytes
    // sector size.
    ms.spawn(async move {
        Lvs::create_or_import(PoolArgs {
            name: "tpool_4k_uring".into(),
            disks: vec![format!("uring://{pool_dev_uring}")],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: None,
        })
        .await
        .unwrap();
    })
    .await;

    // should be able to find our new LVS created on loopdev, and subsequently
    // destroy it.
    ms.spawn(async {
        let pool = Lvs::lookup("tpool_4k_uring").unwrap();
        assert_eq!(pool.name(), "tpool_4k_uring");
        assert_eq!(pool.used(), 0);
        dbg!(pool.uuid());
        pool.destroy().await.unwrap();
    })
    .await;

    // validate the expected state of mayastor
    ms.spawn(async {
        // no shares left except for the discovery controller

        assert_eq!(NvmfSubsystem::first().unwrap().into_iter().count(), 1);

        // all pools destroyed
        assert_eq!(Lvs::iter().count(), 0);

        // no bdevs left

        assert_eq!(UntypedBdev::bdev_first().into_iter().count(), 0);

        // importing a pool with the wrong name should fail
        Lvs::create_or_import(PoolArgs {
            name: "jpool".into(),
            disks: vec![format!("aio://{DISKNAME1}")],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: None,
        })
        .await
        .err()
        .unwrap();
    })
    .await;

    common::delete_file(&[DISKNAME1.into()]);

    // if not specified, default driver scheme should be AIO
    ms.spawn(async {
        let pool = Lvs::create_or_import(PoolArgs {
            name: "tpool2".into(),
            disks: vec![format!("aio://{DISKNAME2}")],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: None,
        })
        .await
        .unwrap();
        assert_eq!(pool.base_bdev().driver(), "aio");
    })
    .await;

    common::delete_file(&[DISKNAME2.into()]);
    common::detach_loopdev(ldev.as_str());
    common::delete_file(&[DISKNAME3.into()]);

    // Create an encrypted pool
    ms.spawn(async {
        let pool = Lvs::create_or_import(PoolArgs {
            name: "enc_pool".into(),
            disks: vec![format!("aio://{DISK_CRYPTO}")],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: Some(EncryptionKey {
                cipher: Cipher::AesXts,
                key_name: "test_key".into(),
                key: XTS_KEY.into(),
                key_len: 128,
                key2: Some(XTS_KEY2.into()),
                key2_len: Some(128),
            }),
            crypto_vbdev_name: Some("crypto_enc_pool".into()),
            raid_config: None,
        })
        .await
        .unwrap();
        let pool_base_bdev = pool.base_bdev();
        assert_eq!(pool_base_bdev.driver(), "crypto");
        let underlying_bdev = pool_base_bdev.crypto_base_bdev().unwrap();
        // we internally use diskname as aio bdev name.
        assert_eq!(underlying_bdev.name(), DISK_CRYPTO);

        // create some replicas on encrypted pool
        let pool = Lvs::lookup("enc_pool").unwrap();
        for i in 0..5 {
            pool.create_lvol(&format!("encvol-{i}"), 8 * 1024 * 1024, None, true, None)
                .await
                .unwrap();
        }
        assert_eq!(pool.lvols().unwrap().count(), 5);
        let dest = pool
            .lvols()
            .unwrap()
            .map(|r| r.destroy())
            .collect::<Vec<_>>();
        assert_eq!(dest.len(), 5);
        futures::future::join_all(dest).await;
        pool.destroy().await.unwrap();
        common::delete_file(&[DISK_CRYPTO.into()]);
    })
    .await;

    // RAID0 Pool Lifecycle Tests
    ms.spawn(async {
        println!("=== RAID0 Pool Creation/Destruction Test - Starting ===");

        // Test 1: Basic RAID0 pool creation with default strip size
        let pool_args = PoolArgs {
            name: "raid0_pool".into(),
            disks: vec![
                "malloc:///raid0_malloc0?size_mb=64".to_string(),
                "malloc:///raid0_malloc1?size_mb=64".to_string(),
            ],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: Some(RaidConfig::Raid0(Raid0Config::default())),
        };

        // Create RAID0 pool
        let pool = Lvs::create_or_import(pool_args).await.unwrap();

        // Verify pool was created successfully
        assert_eq!(pool.name(), "raid0_pool");

        // Verify capacity calculation (should be ~128MB total from 2x64MB devices)
        // Following Task 1 pattern: 128 * 1024 * 1024 = 134,217,728 bytes
        let max_capacity = 128 * 1024 * 1024;
        let actual_capacity = pool.capacity();
        println!(
            "RAID0 pool capacity: {} bytes ({} MB), expected: {} bytes",
            actual_capacity,
            actual_capacity / (1024 * 1024),
            max_capacity
        );

        // Allow some tolerance for metadata overhead (similar to existing pool tests)
        assert!(actual_capacity > 64 * 1024 * 1024);
        assert!(actual_capacity <= max_capacity);

        // Task 4: Validate RAID0 status reporting
        let raid_info = pool.raid_info();
        assert!(raid_info.is_some(), "RAID0 pool should have raid_info");

        let raid_info = raid_info.unwrap();
        assert_eq!(raid_info.level, "raid0", "RAID level should be 'raid0'");
        assert_eq!(raid_info.state, "online", "RAID state should be 'online'");

        println!(
            "RAID0 status validation: level={}, state={}",
            raid_info.level, raid_info.state
        );

        // Verify pool shows up in LVS iterator
        assert_eq!(
            Lvs::iter().filter(|lvs| lvs.name() == "raid0_pool").count(),
            1
        );

        // Clean up: Destroy the pool (this should handle RAID cleanup automatically)
        pool.destroy().await.unwrap();

        // Verify pool is gone
        assert_eq!(
            Lvs::iter().filter(|lvs| lvs.name() == "raid0_pool").count(),
            0
        );

        println!("=== RAID0 Pool Creation/Destruction Test - Completed ===");
    })
    .await;

    // RAID0 Pool Volume Operations Test
    ms.spawn(async {
        println!("=== RAID0 Pool Volume Operations Test - Starting ===");

        // Create RAID0 pool for volume testing
        let pool_args = PoolArgs {
            name: "raid0_vol_pool".into(),
            disks: vec![
                "malloc:///raid0_vol_malloc0?size_mb=64".to_string(),
                "malloc:///raid0_vol_malloc1?size_mb=64".to_string(),
            ],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: Some(RaidConfig::Raid0(Raid0Config::default())),
        };

        let pool = Lvs::create_or_import(pool_args).await.unwrap();

        // Create volumes on RAID0 pool
        let volume_size = 16 * 1024 * 1024; // 16MB volume
        let volume1 = pool
            .create_lvol("raid0_vol_1", volume_size, None, true, None)
            .await
            .unwrap();

        let volume2 = pool
            .create_lvol("raid0_vol_2", volume_size, None, true, None)
            .await
            .unwrap();

        // Verify volumes work correctly
        assert_eq!(pool.lvols().unwrap().count(), 2);
        assert_eq!(volume1.name(), "raid0_vol_1");
        assert_eq!(volume2.name(), "raid0_vol_2");
        assert_eq!(volume1.size(), volume_size);

        // Destroy volumes
        volume1.destroy().await.unwrap();
        volume2.destroy().await.unwrap();
        assert_eq!(pool.lvols().unwrap().count(), 0);

        // Clean up pool
        pool.destroy().await.unwrap();

        println!("=== RAID0 Pool Volume Operations Test - Completed ===");
    })
    .await;

    // RAID0 Pool Export/Import Persistence Test
    ms.spawn(async {
        println!("=== RAID0 Pool Export/Import Test - Starting ===");

        let strip_size_kb = 64;
        let pool_args = PoolArgs {
            name: "raid0_persist_pool".into(),
            disks: vec![
                format!("aio://{}", RAID0_DISK1),
                format!("aio://{}", RAID0_DISK2),
            ],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: Some(RaidConfig::Raid0(Raid0Config { strip_size_kb })),
        };

        // Create RAID0 pool and volume
        let pool = Lvs::create_or_import(pool_args.clone()).await.unwrap();
        let original_uuid = pool.uuid();
        let original_capacity = pool.capacity();

        let volume = pool
            .create_lvol("persist_vol", 16 * 1024 * 1024, None, true, None)
            .await
            .unwrap();

        // Verify volume before export
        assert_eq!(volume.name(), "persist_vol");
        assert_eq!(volume.size(), 16 * 1024 * 1024);
        assert_eq!(pool.lvols().unwrap().count(), 1);

        // Export the pool
        pool.export().await.unwrap();

        // Import the pool back
        let reimported_pool = Lvs::create_or_import(pool_args).await.unwrap();

        // Verify RAID0 configuration persisted
        assert_eq!(reimported_pool.name(), "raid0_persist_pool");
        assert_eq!(reimported_pool.uuid(), original_uuid);
        assert_eq!(reimported_pool.capacity(), original_capacity);

        // Verify volume persisted
        assert_eq!(reimported_pool.lvols().unwrap().count(), 1);
        let persisted_volume = reimported_pool.lvols().unwrap().next().unwrap();
        assert_eq!(persisted_volume.name(), "persist_vol");
        assert_eq!(persisted_volume.size(), 16 * 1024 * 1024);

        // Clean up
        persisted_volume.destroy().await.unwrap();
        reimported_pool.destroy().await.unwrap();

        println!("=== RAID0 Pool Export/Import Test - Completed ===");
    })
    .await;

    // RAID0 Pool I/O Data Integrity Test
    ms.spawn(async {
        println!("=== RAID0 Pool I/O Data Integrity Test - Starting ===");

        let pool_args = PoolArgs {
            name: "raid0_io_pool".into(),
            disks: vec![
                "malloc:///raid0_io_malloc0?size_mb=64".to_string(),
                "malloc:///raid0_io_malloc1?size_mb=64".to_string(),
            ],
            uuid: None,
            cluster_size: None,
            md_args: None,
            backend: PoolBackend::Lvs,
            enc_key: None,
            crypto_vbdev_name: None,
            raid_config: Some(RaidConfig::Raid0(Raid0Config::default())),
        };

        // Create RAID0 pool and volume for I/O testing
        let pool = Lvs::create_or_import(pool_args).await.unwrap();
        let volume = pool
            .create_lvol("io_test_vol", 32 * 1024 * 1024, None, true, None)
            .await
            .unwrap();

        // Get bdev handle for I/O operations (following existing patterns)
        let bdev = volume.as_bdev();
        let handle = UntypedBdev::open_by_name(bdev.name(), true).unwrap();
        let io_handle = handle.into_handle().unwrap();

        // Test 1: Write data spanning multiple strips (2x strip size = 128KB)
        let strip_size = 64 * 1024; // 64KB default strip size
        let write_size = 2 * strip_size; // 128KB spans both devices
        let mut write_buf = io_handle.dma_malloc(write_size).unwrap();

        // Create distinctive striped pattern
        for (i, byte) in write_buf.as_mut_slice().iter_mut().enumerate() {
            *byte = (i % 256) as u8;
        }

        // Write to volume (tests RAID0 striping)
        io_handle.write_at(0, &write_buf).await.unwrap();

        // Test 2: Read back and verify data integrity
        let mut read_buf = io_handle.dma_malloc(write_size).unwrap();
        io_handle.read_at(0, &mut read_buf).await.unwrap();

        // Verify striped data is intact
        let read_slice = read_buf.as_slice();
        for (i, &byte) in read_slice.iter().enumerate() {
            let expected = (i % 256) as u8;
            assert_eq!(
                byte, expected,
                "Data corruption at offset {i} in RAID0 striped volume"
            );
        }

        println!("RAID0 I/O integrity test passed: {write_size} bytes written/read correctly");

        // Test 3: Multiple I/O operations at different offsets
        let test_offsets = [0, strip_size, write_size, write_size + strip_size / 2];
        let small_size = 4096;

        for &offset in &test_offsets {
            let mut test_buf = io_handle.dma_malloc(small_size).unwrap();
            test_buf.fill((offset / 1024) as u8); // Different pattern per offset

            io_handle.write_at(offset, &test_buf).await.unwrap();

            let mut verify_buf = io_handle.dma_malloc(small_size).unwrap();
            io_handle.read_at(offset, &mut verify_buf).await.unwrap();

            for &byte in verify_buf.as_slice() {
                assert_eq!(byte, (offset / 1024) as u8);
            }
        }

        println!("Multiple offset I/O test passed");

        // Clean up (following Task 1 patterns)
        drop(io_handle);
        volume.destroy().await.unwrap();
        pool.destroy().await.unwrap();

        println!("=== RAID0 Pool I/O Data Integrity Test - Completed ===");
    })
    .await;
}
