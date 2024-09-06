#[cfg(not(feature = "rapid_debouncer"))]
use crate::debounce::default_bouncer::DefaultDebouncer;
#[cfg(feature = "rapid_debouncer")]
use crate::debounce::fast_debouncer::RapidDebouncer;
use crate::matrix::{Matrix, MatrixTrait};
use embedded_hal::digital::{InputPin, OutputPin};
#[cfg(feature = "async_matrix")]
use embedded_hal_async::digital::Wait;
#[cfg(feature = "_nrf_ble")]
use {
    crate::ble::nrf::softdevice_task,
    core::mem,
    embassy_executor::Spawner,
    nrf_softdevice::ble::gatt_server::set_sys_attrs,
    nrf_softdevice::ble::peripheral::{advertise_connectable, ConnectableAdvertisement},
    nrf_softdevice::ble::{set_address, Address, AddressType},
    nrf_softdevice::{raw, Config, Softdevice},
};

use super::{
    driver::{SplitReader, SplitWriter},
    SplitMessage,
};

#[cfg(not(feature = "_nrf_ble"))]
use {
    super::serial::SerialSplitDriver,
    embedded_io_async::{Read, Write},
};

pub async fn run_rmk_split_slave<
    #[cfg(feature = "async_matrix")] In: Wait + InputPin,
    #[cfg(not(feature = "async_matrix"))] In: InputPin,
    Out: OutputPin,
    #[cfg(not(feature = "_nrf_ble"))] S: Write + Read,
    const ROW: usize,
    const COL: usize,
>(
    #[cfg(feature = "col2row")] input_pins: [In; ROW],
    #[cfg(not(feature = "col2row"))] input_pins: [In; COL],
    #[cfg(feature = "col2row")] output_pins: [Out; COL],
    #[cfg(not(feature = "col2row"))] output_pins: [Out; ROW],
    #[cfg(feature = "_nrf_ble")] master_addr: [u8; 6],
    #[cfg(feature = "_nrf_ble")] slave_addr: [u8; 6],
    #[cfg(not(feature = "_nrf_ble"))] serial: S,
    #[cfg(feature = "_nrf_ble")] spawner: Spawner,
) {
    #[cfg(not(feature = "_nrf_ble"))]
    initialize_serial_split_slave_and_run::<In, Out, S, ROW, COL>(
        input_pins, output_pins, serial,
    ).await;

    #[cfg(feature = "_nrf_ble")]
    initialize_nrf_ble_split_slave_and_run::<In, Out, ROW, COL>(
        input_pins, output_pins, master_addr, slave_addr, spawner,
    ).await;
}

/// Initialize and run the nRF slave keyboard service via BLE.
///
/// # Arguments
///
/// * `input_pins` - input gpio pins
/// * `output_pins` - output gpio pins
/// * `spwaner` - embassy task spwaner, used to spawn nrf_softdevice background task
#[cfg(feature = "_nrf_ble")]
pub(crate) async fn initialize_nrf_ble_split_slave_and_run<
    #[cfg(feature = "async_matrix")] In: Wait + InputPin,
    #[cfg(not(feature = "async_matrix"))] In: InputPin,
    Out: OutputPin,
    const ROW: usize,
    const COL: usize,
>(
    #[cfg(feature = "col2row")] input_pins: [In; ROW],
    #[cfg(not(feature = "col2row"))] input_pins: [In; COL],
    #[cfg(feature = "col2row")] output_pins: [Out; COL],
    #[cfg(not(feature = "col2row"))] output_pins: [Out; ROW],
    master_addr: [u8; 6],
    slave_addr: [u8; 6],
    spawner: Spawner,
) -> ! {
    use defmt::info;
    use embassy_futures::select::select;
    use nrf_softdevice::ble::gatt_server;

    use crate::split::nrf::slave::{
        BleSplitSlaveDriver, BleSplitSlaveServer, BleSplitSlaveServerEvent, SplitBleServiceEvent,
    };

    // Keyboard matrix, use COL2ROW by default
    #[cfg(all(feature = "col2row", feature = "rapid_debouncer"))]
    let mut matrix =
        Matrix::<_, _, RapidDebouncer<ROW, COL>, ROW, COL>::new(input_pins, output_pins);
    #[cfg(all(feature = "col2row", not(feature = "rapid_debouncer")))]
    let mut matrix =
        Matrix::<_, _, DefaultDebouncer<ROW, COL>, ROW, COL>::new(input_pins, output_pins);
    #[cfg(all(not(feature = "col2row"), feature = "rapid_debouncer"))]
    let mut matrix =
        Matrix::<_, _, RapidDebouncer<COL, ROW>, COL, ROW>::new(input_pins, output_pins);
    #[cfg(all(not(feature = "col2row"), not(feature = "rapid_debouncer")))]
    let mut matrix =
        Matrix::<_, _, DefaultDebouncer<COL, ROW>, COL, ROW>::new(input_pins, output_pins);

    let ble_config = Config {
        clock: Some(raw::nrf_clock_lf_cfg_t {
            source: raw::NRF_CLOCK_LF_SRC_RC as u8,
            rc_ctiv: 16,
            rc_temp_ctiv: 2,
            accuracy: raw::NRF_CLOCK_LF_ACCURACY_500_PPM as u8,
            // External osc
            // source: raw::NRF_CLOCK_LF_SRC_XTAL as u8,
            // rc_ctiv: 0,
            // rc_temp_ctiv: 0,
            // accuracy: raw::NRF_CLOCK_LF_ACCURACY_20_PPM as u8,
        }),
        conn_gap: Some(raw::ble_gap_conn_cfg_t {
            conn_count: 6,
            event_length: 24,
        }),
        conn_gatt: Some(raw::ble_gatt_conn_cfg_t { att_mtu: 256 }),
        gatts_attr_tab_size: Some(raw::ble_gatts_cfg_attr_tab_size_t {
            attr_tab_size: raw::BLE_GATTS_ATTR_TAB_SIZE_DEFAULT,
        }),
        gap_role_count: Some(raw::ble_gap_cfg_role_count_t {
            adv_set_count: 1,
            periph_role_count: 4,
            central_role_count: 4,
            central_sec_count: 4,
            _bitfield_1: raw::ble_gap_cfg_role_count_t::new_bitfield_1(0),
        }),
        gap_device_name: Some(raw::ble_gap_cfg_device_name_t {
            p_value: "rmk_slave_board".as_ptr() as _,
            current_len: "rmk_slave_board".len() as u16,
            max_len: "rmk_slave_board".len() as u16,
            write_perm: unsafe { mem::zeroed() },
            _bitfield_1: raw::ble_gap_cfg_device_name_t::new_bitfield_1(
                raw::BLE_GATTS_VLOC_STACK as u8,
            ),
        }),
        ..Default::default()
    };

    let sd = Softdevice::enable(&ble_config);
    set_address(sd, &Address::new(AddressType::RandomStatic, slave_addr));

    {
        // Use the immutable ref of `Softdevice` to run the softdevice_task
        // The mumtable ref is used for configuring Flash and BleServer
        let sdv = unsafe { nrf_softdevice::Softdevice::steal() };
        defmt::unwrap!(spawner.spawn(softdevice_task(sdv)))
    };

    let server = defmt::unwrap!(BleSplitSlaveServer::new(sd));

    loop {
        let advertisement = ConnectableAdvertisement::NonscannableDirected {
            peer: Address::new(AddressType::RandomStatic, master_addr),
        };
        let conn = match advertise_connectable(sd, advertisement, &Default::default()).await {
            Ok(conn) => conn,
            Err(e) => {
                defmt::error!("Split slave advertise error: {}", e);
                continue;
            }
        };

        // Set sys attr of slave
        set_sys_attrs(&conn, None).unwrap();

        let server_fut = gatt_server::run(&conn, &server, |event| match event {
            BleSplitSlaveServerEvent::Service(split_event) => match split_event {
                SplitBleServiceEvent::MessageToCentralCccdWrite { notifications } => {
                    info!("Split value CCCD updated: {}", notifications)
                }
                SplitBleServiceEvent::MessageToPeripheralWrite(message) => {
                    // TODO: Handle message from master to slave
                    info!("Message from master: {:?}", message);
                }
            },
        });

        let mut slave = SplitSlave::new(&mut matrix, BleSplitSlaveDriver::new(&server, &conn));
        let slave_fut = slave.run();
        select(server_fut, slave_fut).await;
    }
}

/// Initialize and run the slave keyboard service via serial.
///
/// # Arguments
///
/// * `input_pins` - input gpio pins
/// * `output_pins` - output gpio pins
/// * `serial` - serial port to send key events to master board
#[cfg(not(feature = "_nrf_ble"))]
pub(crate) async fn initialize_serial_split_slave_and_run<
    #[cfg(feature = "async_matrix")] In: Wait + InputPin,
    #[cfg(not(feature = "async_matrix"))] In: InputPin,
    Out: OutputPin,
    S: Write + Read,
    const ROW: usize,
    const COL: usize,
>(
    #[cfg(feature = "col2row")] input_pins: [In; ROW],
    #[cfg(not(feature = "col2row"))] input_pins: [In; COL],
    #[cfg(feature = "col2row")] output_pins: [Out; COL],
    #[cfg(not(feature = "col2row"))] output_pins: [Out; ROW],
    serial: S,
) -> ! {
    // Keyboard matrix, use COL2ROW by default
    #[cfg(all(feature = "col2row", feature = "rapid_debouncer"))]
    let mut matrix =
        Matrix::<_, _, RapidDebouncer<ROW, COL>, ROW, COL>::new(input_pins, output_pins);
    #[cfg(all(feature = "col2row", not(feature = "rapid_debouncer")))]
    let mut matrix =
        Matrix::<_, _, DefaultDebouncer<ROW, COL>, ROW, COL>::new(input_pins, output_pins);
    #[cfg(all(not(feature = "col2row"), feature = "rapid_debouncer"))]
    let mut matrix =
        Matrix::<_, _, RapidDebouncer<COL, ROW>, COL, ROW>::new(input_pins, output_pins);
    #[cfg(all(not(feature = "col2row"), not(feature = "rapid_debouncer")))]
    let mut matrix =
        Matrix::<_, _, DefaultDebouncer<COL, ROW>, COL, ROW>::new(input_pins, output_pins);

    let mut slave = SplitSlave::new(&mut matrix, SerialSplitDriver::new(serial));
    slave.run().await
}

/// The split slave instance.
pub(crate) struct SplitSlave<'a, M: MatrixTrait, S: SplitWriter + SplitReader> {
    matrix: &'a mut M,
    split_driver: S,
}

impl<'a, M: MatrixTrait, S: SplitWriter + SplitReader> SplitSlave<'a, M, S> {
    pub(crate) fn new(matrix: &'a mut M, split_driver: S) -> Self {
        Self {
            matrix,
            split_driver,
        }
    }

    /// Run the slave keyboard service.
    ///
    /// The slave uses the general matrix, does scanning and send the key events through `SplitWriter`.
    /// If also receives split messages from the master through `SplitReader`.
    pub(crate) async fn run(&mut self) -> ! {
        loop {
            self.matrix.scan().await;

            for row_idx in 0..self.matrix.get_row_num() {
                for col_idx in 0..self.matrix.get_col_num() {
                    let key_state = self.matrix.get_key_state(row_idx, col_idx);
                    if key_state.changed {
                        let _ = self
                            .split_driver
                            .write(&SplitMessage::Key(
                                row_idx as u8,
                                col_idx as u8,
                                key_state.pressed,
                            ))
                            .await;
                    }
                }
            }

            // 10KHZ scan rate
            embassy_time::Timer::after_micros(10).await;
        }
    }
}

/// Run the slave keyboard service.
///
/// The slave uses the general matrix, does scanning and send the key events through `SplitWriter`.
/// If also receives split messages from the master through `SplitReader`.
pub(crate) async fn run_slave<
    M: MatrixTrait,
    S: SplitWriter + SplitReader,
    const ROW: usize,
    const COL: usize,
>(
    matrix: &mut M,
    mut split_driver: S,
) -> ! {
    loop {
        matrix.scan().await;

        // Send key events to host
        for row_idx in 0..matrix.get_row_num() {
            for col_idx in 0..matrix.get_col_num() {
                let key_state = matrix.get_key_state(row_idx, col_idx);
                if key_state.changed {
                    let _ = split_driver
                        .write(&SplitMessage::Key(
                            row_idx as u8,
                            col_idx as u8,
                            key_state.pressed,
                        ))
                        .await;
                }
            }
        }

        // 10KHZ scan rate
        embassy_time::Timer::after_micros(10).await;
    }
}
