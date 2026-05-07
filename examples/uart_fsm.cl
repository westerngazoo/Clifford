// uart_fsm.cl — minimal multi-state firmware example for v0.1 codegen
// smoke testing. Lowered to LLVM IR via `cliffordc compile uart_fsm.cl`.
//
// Two automatons:
//   * `Uart` (register block) at MMIO base 0x4000_4000 — `tx_data` and
//     `status` are MMIO mapped; `send` writes a byte to `tx_data` via
//     `store volatile`.
//   * `TxFsm` (multi-state) tracks the driver state. Starts in `Idle`,
//     `start` transitions to `Sending`, `finish` to `Done`.
//
// One interrupt:
//   * `USART1_IRQ` calls `Uart_send()` when invoked. Section attribute
//     `.interrupts` is added by the codegen so the linker can place
//     this in the vector table.

#automaton Uart {
  #address: 0x4000_4000;
  tx_data: u32 #offset: 0x00;
  status:  u32 #offset: 0x18;
  #transition send { Uart.tx_data = 65u32; }   // 'A'
}

#automaton TxFsm {
  #states: [Idle, Sending, Done];
  bytes_sent: u32;
  #transition start  -> Sending { TxFsm.bytes_sent = 0u32; }
  #transition tick               { TxFsm.bytes_sent += 1u32; }
  #transition finish -> Done    $ [Release] { return; }
}

#effect drain() #mutates: [TxFsm] {
  TxFsm.bytes_sent += 1u32;
}

#effect peek_state() -> u32 #mutates: [TxFsm] {
  return TxFsm@state;
}

#interrupt USART1_IRQ() #mutates: [Uart] #priority: HIGH {
  #> send();
}
