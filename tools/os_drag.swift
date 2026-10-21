// A left-button drag posted as operating-system mouse events, at a precise rate.
//
//   swiftc -O tools/os_drag.swift -o /tmp/os_drag
//   /tmp/os_drag <x> <y> <dx> <ms> <hz>    # screen points; drag dx over ms at hz
//
// `firefox_probe --drag-os` runs it. Synthesized browser input (WebDriver BiDi,
// CDP) is dispatched on the browser's own timers, so its timestamps are as
// uneven as its delivery and it cannot tell a pacing fault from its own jitter.
// These go through the window server like a mouse's, with the time each was
// posted, at an even 1/hz.
//
// The event source is nil on purpose: from an ssh session, creating a
// `.hidSystemState` source deadlocks inside SkyLight (`CGSEventSourceShutdown`
// waiting on a mutex, 2026-10-21), and nil posts fine.
import CoreGraphics
import Foundation

let a = CommandLine.arguments
guard a.count == 6, let x = Double(a[1]), let y = Double(a[2]), let dx = Double(a[3]),
  let ms = Double(a[4]), let hz = Double(a[5])
else {
  print("usage: os_drag <x> <y> <dx> <ms> <hz>")
  exit(2)
}
func post(_ type: CGEventType, _ px: Double) {
  CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: CGPoint(x: px, y: y), mouseButton: .left)?
    .post(tap: .cghidEventTap)
}
post(.mouseMoved, x)
usleep(50_000)
post(.leftMouseDown, x)
let steps = max(1, Int(ms * hz / 1000))
let start = DispatchTime.now().uptimeNanoseconds
for i in 1...steps {
  let due = start + UInt64(Double(i) * 1e9 / hz)
  while DispatchTime.now().uptimeNanoseconds < due { usleep(200) }
  post(.leftMouseDragged, x + dx * Double(i) / Double(steps))
}
post(.leftMouseUp, x + dx)
print("posted \(steps) drag events")
