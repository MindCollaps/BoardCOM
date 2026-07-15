//! Hardware resource pool: turns config-level resource numbers (GPIO pins,
//! peripheral controllers) into owned esp-idf-hal driver handles, exactly
//! once.
//!
//! Drivers never touch `Peripherals` directly; they claim what their config
//! names from this pool, and double-claims fail with a meaningful error
//! instead of a panic or silent aliasing.

use std::collections::HashSet;

use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver, I2C0};
use esp_idf_svc::hal::pcnt::config::UnitConfig;
use esp_idf_svc::hal::pcnt::PcntUnitDriver;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::spi::config::DriverConfig;
use esp_idf_svc::hal::spi::{config as spi_config, Dma, SpiDeviceDriver, SpiDriver, SPI2};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::{EspError, ESP_ERR_NOT_FOUND};

use crate::error::DriverError;

pub struct HardwarePool {
    /// GPIO numbers already handed out. Constructing the pool consumed the
    /// singleton `Peripherals` (dropping every typed pin), so guarding the
    /// unsafe `AnyIOPin::steal` below with this set keeps pin access unique.
    claimed_pins: HashSet<u8>,
    i2c0: Option<I2C0<'static>>,
    spi2: Option<SPI2<'static>>,
}

impl HardwarePool {
    /// Consumes the singleton `Peripherals`: typed peripherals this pool
    /// hands out are moved in, everything else is dropped, so this pool is
    /// the only remaining source of hardware handles.
    pub fn new(peripherals: Peripherals) -> Self {
        Self {
            claimed_pins: HashSet::new(),
            i2c0: Some(peripherals.i2c0),
            spi2: Some(peripherals.spi2),
        }
    }

    /// Claim a GPIO by number. Fails if out of range or already claimed.
    pub fn claim_pin(&mut self, gpio: i32) -> Result<AnyIOPin<'static>, DriverError> {
        // GPIOs 34-39 are input-only on the ESP32 but still valid to claim;
        // the driver using them decides direction.
        let pin: u8 = gpio
            .try_into()
            .ok()
            .filter(|p| (0..=39).contains(p))
            .ok_or_else(|| {
                DriverError::ResourceConflict(format!("GPIO {gpio} does not exist on the ESP32"))
            })?;
        if !self.claimed_pins.insert(pin) {
            return Err(DriverError::ResourceConflict(format!(
                "GPIO {gpio} is already in use by another entity"
            )));
        }
        // SAFETY: the typed pin singletons were consumed with `Peripherals`
        // when this pool was built, and `claimed_pins` guarantees each number
        // is handed out at most once, so no aliased pin handles can exist.
        Ok(unsafe { AnyIOPin::steal(pin) })
    }

    /// Claim a PCNT unit. ESP-IDF allocates units internally; exhaustion
    /// (more pulse-counting entities than hardware units) is reported as a
    /// resource conflict rather than a raw ESP error.
    pub fn claim_pcnt_unit(
        &mut self,
        config: &UnitConfig,
    ) -> Result<PcntUnitDriver<'static>, DriverError> {
        PcntUnitDriver::new(config).map_err(|e: EspError| {
            if e.code() == ESP_ERR_NOT_FOUND {
                DriverError::ResourceConflict("all PCNT units are already in use".to_owned())
            } else {
                DriverError::Esp(e)
            }
        })
    }

    /// Claim the SPI2 (HSPI) controller as a single write-only device on the
    /// given pins. MISO is deliberately not wired: the only SPI peripherals
    /// in the system are displays, which are write-only.
    pub fn claim_spi(
        &mut self,
        sclk: i32,
        mosi: i32,
        cs: i32,
        baudrate: Hertz,
    ) -> Result<SpiDeviceDriver<'static, SpiDriver<'static>>, DriverError> {
        let sclk = self.claim_pin(sclk)?;
        let mosi = self.claim_pin(mosi)?;
        let cs = self.claim_pin(cs)?;
        let spi2 = self
            .spi2
            .take()
            .ok_or_else(|| DriverError::ResourceConflict("SPI2 is already in use".to_owned()))?;
        let config = spi_config::Config::new().baudrate(baudrate);
        // Without DMA, esp-idf splits every transfer into 64-byte FIFO
        // transactions, so a bulk display blit (tens of KB) costs thousands
        // of transactions and blocks the calling task for seconds. 4096
        // matches the largest chunk the display drivers hand over per write.
        let driver_config = DriverConfig::new().dma(Dma::Auto(4096));
        Ok(SpiDeviceDriver::new_single(
            spi2,
            sclk,
            mosi,
            None::<AnyIOPin>,
            Some(cs),
            &driver_config,
            &config,
        )?)
    }

    /// Claim the I2C0 controller on the given pins.
    pub fn claim_i2c(
        &mut self,
        sda: i32,
        scl: i32,
        baudrate: Hertz,
    ) -> Result<I2cDriver<'static>, DriverError> {
        let sda = self.claim_pin(sda)?;
        let scl = self.claim_pin(scl)?;
        let i2c0 = self
            .i2c0
            .take()
            .ok_or_else(|| DriverError::ResourceConflict("I2C0 is already in use".to_owned()))?;
        let config = I2cConfig::new().baudrate(baudrate);
        Ok(I2cDriver::new(i2c0, sda, scl, &config)?)
    }
}
