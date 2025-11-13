use common::MayastorTest;
use io_engine::{
    bdev_api::{bdev_create, bdev_destroy},
    core::{MayastorCliArgs, UntypedBdev},
};
use spdk_rs::DmaBuf;

pub mod common;

#[tokio::test]
async fn raid0_bdev_integration() {
    let ms = MayastorTest::new(MayastorCliArgs {
        reactor_mask: "0x1".to_string(),
        no_pci: true,
        mem_size: 128,
        ..Default::default()
    });

    // Test 1: Basic create/destroy test
    ms.spawn(async {
        println!("=== Test 1: Basic create/destroy test - Starting ===");

        // Test 1 device URIs - unique names to avoid conflicts
        let malloc0_uri = "malloc:///test1_malloc0?blk_size=512&size_mb=64";
        let malloc1_uri = "malloc:///test1_malloc1?blk_size=512&size_mb=64";
        let raid0_uri = "raid0:///test1_raid0?strip_size=64&children=test1_malloc0,test1_malloc1";

        // Create child devices first
        bdev_create(malloc0_uri).await.unwrap();
        bdev_create(malloc1_uri).await.unwrap();

        // Create RAID0 bdev using existing child devices
        bdev_create(raid0_uri).await.unwrap();

        // Verify it was created
        assert!(UntypedBdev::lookup_by_name("test1_raid0").is_some());

        // Destroy RAID0 bdev
        bdev_destroy(raid0_uri).await.unwrap();

        // Verify RAID0 bdev is gone
        assert!(UntypedBdev::lookup_by_name("test1_raid0").is_none());

        // Clean up child devices
        bdev_destroy(malloc0_uri).await.unwrap();
        bdev_destroy(malloc1_uri).await.unwrap();

        // Verify child devices are gone
        assert!(UntypedBdev::lookup_by_name("test1_malloc0").is_none());
        assert!(UntypedBdev::lookup_by_name("test1_malloc1").is_none());
        println!("=== Test 1: Basic create/destroy test - Completed ===");
    })
    .await;

    // Test 2: Verify RAID0 bdev properties (driver, capacity, striping)
    ms.spawn(async {
        println!("=== Test 2: Properties test - Starting ===");

        // Test 2 device URIs - unique names to avoid conflicts
        let malloc0_uri = "malloc:///test2_malloc0?blk_size=512&size_mb=64";
        let malloc1_uri = "malloc:///test2_malloc1?blk_size=512&size_mb=64";
        let raid0_uri = "raid0:///test2_raid0?strip_size=64&children=test2_malloc0,test2_malloc1";

        // Create child devices first
        bdev_create(malloc0_uri).await.unwrap();
        bdev_create(malloc1_uri).await.unwrap();

        // Create RAID0 bdev using existing child devices
        bdev_create(raid0_uri).await.unwrap();

        // Open the RAID0 bdev and verify its properties
        let bdev = UntypedBdev::open_by_name("test2_raid0", true).unwrap();
        let raid0_bdev = bdev.bdev().as_raid_bdev().expect("Should be a RAID bdev");

        // Get capacity from both UntypedBdev and RaidBdev methods
        let raid_devices_count = raid0_bdev.num_bdevs();
        let raid_capacity = raid0_bdev.capacity();
        let expected_raid_capacity = 128 * 1024 * 1024;

        println!("RAID0 capacity comparison:");
        println!("  Raid devices count: {raid_devices_count}");
        println!(
            "  Raid capacity: {} bytes ({} MB)",
            raid_capacity,
            raid_capacity / (1024 * 1024)
        );

        assert_eq!(raid_devices_count, 2);
        assert_eq!(raid_capacity, expected_raid_capacity);

        // Verify block size matches child devices (512 bytes as specified in URIs)
        assert_eq!(bdev.bdev().block_len(), 512);

        // Verify this is a RAID bdev
        assert_eq!(bdev.bdev().driver(), "raid");

        // Drop handles before cleanup
        drop(bdev);

        // Clean up
        bdev_destroy(raid0_uri).await.unwrap();
        bdev_destroy(malloc0_uri).await.unwrap();
        bdev_destroy(malloc1_uri).await.unwrap();

        println!("=== Test 2: Properties test - Completed ===");
    })
    .await;

    // Test 3: I/O operations test to verify striping functionality
    ms.spawn(async {
        println!("=== Test 3: I/O operations test - Starting ===");

        // Test 3 device URIs - unique names to avoid conflicts
        let malloc0_uri = "malloc:///test3_malloc0?blk_size=512&size_mb=64";
        let malloc1_uri = "malloc:///test3_malloc1?blk_size=512&size_mb=64";
        let raid0_uri = "raid0:///test3_raid0?strip_size=64&children=test3_malloc0,test3_malloc1";

        // Create child devices first
        bdev_create(malloc0_uri).await.unwrap();
        bdev_create(malloc1_uri).await.unwrap();

        // Create RAID0 bdev using existing child devices
        bdev_create(raid0_uri).await.unwrap();

        let bdev = UntypedBdev::open_by_name("test3_raid0", true).unwrap();
        let raid0_handle = bdev.into_handle().unwrap();

        // Test basic I/O operations to verify striping
        let mut buf = DmaBuf::new(4096, 9).unwrap();
        buf.fill(42);

        // Write data to RAID0 bdev
        raid0_handle.write_at(0, &buf).await.unwrap();

        // Read back and verify
        let mut read_buf = raid0_handle.dma_malloc(4096).unwrap();
        raid0_handle.read_at(0, &mut read_buf).await.unwrap();

        let read_slice = read_buf.as_slice();
        for &byte in read_slice {
            assert_eq!(byte, 42);
        }

        // Drop handle before cleanup
        drop(raid0_handle);

        // Clean up
        bdev_destroy(raid0_uri).await.unwrap();
        bdev_destroy(malloc0_uri).await.unwrap();
        bdev_destroy(malloc1_uri).await.unwrap();

        println!("=== Test 3: I/O operations test - Completed ===");
    })
    .await;

    // Test 4: Re-open and read test to verify RAID0 'import' functionality
    ms.spawn(async {
        println!("=== Test 4: Re-open and read test - Starting ===");

        // Test 4 device URIs - unique names to avoid conflicts
        let malloc0_uri = "malloc:///test4_malloc0?blk_size=512&size_mb=64";
        let malloc1_uri = "malloc:///test4_malloc1?blk_size=512&size_mb=64";
        let raid0_uri = "raid0:///test4_raid0?strip_size=64&children=test4_malloc0,test4_malloc1";

        // Create child devices for import test
        bdev_create(malloc0_uri).await.unwrap();
        bdev_create(malloc1_uri).await.unwrap();

        // Create RAID0 bdev using existing child devices
        bdev_create(raid0_uri).await.unwrap();

        let bdev = UntypedBdev::open_by_name("test4_raid0", true).unwrap();
        let raid0_handle = bdev.into_handle().unwrap();

        // Write distinctive pattern
        let mut write_buf = DmaBuf::new(8192, 9).unwrap();
        for (i, byte) in write_buf.as_mut_slice().iter_mut().enumerate() {
            *byte = (i % 256) as u8;
        }
        raid0_handle.write_at(0, &write_buf).await.unwrap();

        // Close and re-open to simulate 'import' scenario
        drop(raid0_handle);

        // Re-open the same RAID0 bdev
        let reimported_bdev = UntypedBdev::open_by_name("test4_raid0", true).unwrap();
        let reimported_handle = reimported_bdev.into_handle().unwrap();

        // Read back data to verify persistence/import worked
        let mut read_buf = reimported_handle.dma_malloc(8192).unwrap();
        reimported_handle.read_at(0, &mut read_buf).await.unwrap();

        // Verify data matches original pattern
        let read_slice = read_buf.as_slice();
        for (i, &byte) in read_slice.iter().enumerate() {
            assert_eq!(byte, (i % 256) as u8, "Data mismatch at byte {i}");
        }

        // Drop handle before cleanup
        drop(reimported_handle);

        // Clean up import test devices
        bdev_destroy(raid0_uri).await.unwrap();
        bdev_destroy(malloc0_uri).await.unwrap();
        bdev_destroy(malloc1_uri).await.unwrap();

        println!("=== Test 4: Re-open and read test - Completed ===");
    })
    .await;

    // Test 5: Integration failure scenarios
    ms.spawn(async {
        println!("=== Test 5: Integration failure scenarios - Starting ===");

        // Test 5 device URIs - unique names to avoid conflicts
        let malloc0_uri = "malloc:///test5_malloc0?blk_size=512&size_mb=64";
        let malloc1_uri = "malloc:///test5_malloc1?blk_size=512&size_mb=64";
        let raid0_uri = "raid0:///test5_raid0?strip_size=64&children=test5_malloc0,test5_malloc1";

        // Test missing child devices - integration failure
        let result = bdev_create(
            "raid0:///missing_children?strip_size=64&children=nonexistent1,nonexistent2",
        )
        .await;
        assert!(result.is_err());

        // Test duplicate bdev name - integration failure
        // Create child devices for duplicate test
        bdev_create(malloc0_uri).await.unwrap();
        bdev_create(malloc1_uri).await.unwrap();
        bdev_create(raid0_uri).await.unwrap();

        // Second create should fail as raid exists already
        let result = bdev_create(raid0_uri).await;
        assert!(result.is_err());

        // Cleanup duplicate test devices
        bdev_destroy(raid0_uri).await.unwrap();
        bdev_destroy(malloc0_uri).await.unwrap();
        bdev_destroy(malloc1_uri).await.unwrap();

        println!("=== Test 5: Integration failure scenarios - Completed ===");
    })
    .await;
}
