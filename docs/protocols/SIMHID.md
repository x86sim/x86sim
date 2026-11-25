# simhid: touch, buttons, keyboard and the simulator's other inputs

`host/simhid/simhid.m` builds `SimulatorHID.framework`, a stand-in for CoreSimulator's SimulatorHID. backboardd loads
it from `SIMULATOR_HID_SYSTEM_MANAGER` (SimulatorClient's `IndigoHIDSystemSpawnLoopback`): the bundle's principal
class `SimHIDSystem` is created with backboardd's `IOHIDEventSystemRef` and adds virtual HID services to it
(`_IOHIDServiceCreateVirtual`, `_IOHIDEventSystemAddService`, `_IOHIDServiceDispatchEvent`): a built-in
touchscreen, the main-screen buttons (home, lock, volume) and a keyboard. Apple's version receives "Indigo" messages
from Simulator.app over a mach port; ours reads a Unix socket.

Clients: `cli/libexec/simview.py` (mouse, keyboard; Ctrl/Shift+drag = two fingers; F1/Ctrl+H home, F2 lock, F3/F4
volume) and `cli/libexec/simtouch.py` (`x86sim tap|swipe|pinch|pan2|key|type|home|lock|crown|button|shake|rotate|
orientation|biometric|gamepad|memwarn|raw`); the launcher's MCP server has sim_button, sim_crown, sim_shake,
sim_orientation, sim_biometric, sim_gamepad and sim_memory_warning. Device pixel size comes
from the live frame ring / `SIMULATOR_MAINSCREEN_*` (arm64 is 1206×2622).

Arm64 notes: the touchscreen HID props must include `displayUUID` = `x86Sim-main-display` (same as
`SFBDisplayGetDisplayUID`); `BKIOHIDService` copies that property into its displayUUID ivar, and without it
`BKDisplayConvertCAScreenLocationToContext` drops every touch. Dispatch `_IOHIDServiceDispatchEvent` on the
socket thread (not `dispatch_async`) and serialize `handle_line` with a mutex (`dispatch_sync` was a silent
no-op under the A64 JIT).

## Socket protocol

```
Socket: $SIMHID_SOCKET (default <SIMULATOR_SHARED_RESOURCES_DIRECTORY>/simhid.sock), stream, any number of
clients, one text command per line. A path too long for sun_path (104 bytes, Darling's /Volumes/SystemRoot included)
is a symlink to /run/user/<uid>/x86sim-simhid-<SIMULATOR_UDID>.sock (next to the frame ring); clients resolve it
before connecting (Linux's own limit is 108):
  touch down|move|up <x> <y> [edge]   x, y normalized to the display (0..1, origin top left); edge (on down, kept
                                 for the whole touch): none|top|left|bottom|right, a touch that begins at that screen
                                 edge (system gestures: home indicator, Control/Notification Center)
  touch2 down|move|up <x1> <y1> <x2> <y2>   two fingers (pinch, two-finger pan): one hand event with two finger
                                 children, finger indexes 1 and 2 with their own identities
  @<screenID> <touch command>    the touch on another integrated display (iPhone Duo: @3 = the inner "LCD-1"; x, y
                                 normalized to that display's framebuffer as simfb publishes it)
  hinge <degrees> [ms]           devices with two integrated displays: the hinge angle, 0 folded (closed) .. 180 flat
  fold | unfold                  (open), moved there over <ms> (default 600) in 100 Hz reports; fold / unfold = hinge
                                 0 / 180. CoreMotion reads it from the hinge service (vendor page 0xff0c, usage 0x21)
                                 and SpringBoard's device pose picks the outer or inner display
  key down|up <usage> [page]     keyboard usage (page 7), e.g. 4 = 'a', 0x28 = return; another page's key on the
                                 keyboard service: key down 0x1ae 0x0c = the on-screen keyboard key (Consumer AL Keyboard
                                 Layout), Apple's Toggle Software Keyboard (UIKit flips its minimized keyboard while a
                                 hardware keyboard is connected; Consumer 0xb8 Eject did the same in Apple's simulator)
  hwkb connect|disconnect        Connect Hardware Keyboard, as CoreSimulator's -[SimDevice setHardwareKeyboardEnabled:
                                 keyboardType:error:]: CoreSimulatorBridge calls GraphicsServices'
                                 GSEventSetHardwareKeyboardAttachedWithCountryCodeAndType, i.e. the state of the darwin
                                 notification GSEventHardwareKeyboardAttached (bit 0 attached, country << 8, type << 16)
                                 + a post (Apple's iOS 27.2 simulator: 1 at boot, 0 after a disconnect). Disconnected,
                                 UIKit shows the software keyboard on every focus and keeps it up while keys type.
                                 backboardd sets the state to 1 whenever a keyboard service arrives (our on-demand one
                                 at its first key): simhid watches it and puts 0 back while disconnected. Kept in
                                 <data>/hwkeyboard (connected | disconnected) and applied again at the next boot
  kbidle <seconds>               this client's keyboard idle detach: after its last key the on-demand keyboard stays
                                 attached that long (default SIMHID_KB_IDLE_S, 30; <= 0 never detaches)
  kbsettle <ms>                  this client's wait after the keyboard service is attached (default
                                 SIMHID_KB_SETTLE_MS, 150; the first attach of a boot SIMHID_KB_FIRST_SETTLE_MS, 800:
                                 UIKit's switch to hardware-keyboard mode is cold then, and a Shift typed 150 ms after
                                 it turned that whole burst into capitals)
  button home|lock|volup|voldown down|up
  button menu|tv|playpause|side|siri|action|wallet|select|back|mute down|up
                                 Apple's usages per platform (Inputs 2 table below)
  usage <page> <usage> down|up   any page / usage on the buttons service (for finding what a platform reads)
  crown <delta> [x|y|z]          Apple Watch: the Digital Crown turned by <delta> (a scroll event on that axis,
                                 default y; positive = down). The crown service is Apple's: vendor page 0xff0b,
                                 usage 1, built in. Vision Pro: the same scroll event on its Dial service
                                 (GenericDesktop 0x37, as Apple's IndigoHIDMessageForDigitalDialEvent)
  button <name> down|up          Inputs 2: Apple's buttons per platform (table below)
  shake                          Device > Shake: the darwin notification com.apple.UIKit.SimulatorShake (UIKit sends
                                 a shake motion event to the key window's first responder)
  orientation [<name>|1..6]      the device orientation (portrait portraitUpsideDown landscapeLeft landscapeRight faceUp
  rotate left|right              faceDown), as Xcode's in-device dthidd sets it: BackBoardServices'
                                 BKSHIDServicesSetDeviceOrientationForAutomation; left = counterclockwise (portrait ->
                                 landscapeLeft -> portraitUpsideDown -> landscapeRight). iPhone and iPad only.
                                 The result goes to <data>/orientation. The framebuffer stays the panel's (landscape
                                 apps are drawn sideways in it, as in Apple's framebuffer; Simulator turns its window)
  biometric enroll|unenroll|match|nomatch [face|touch|optic]
                                 Face ID / Touch ID / Optic ID as SimulatorCoreDevicePlugin does them: enrollment =
                                 the state of com.apple.BiometricKit.enrollmentChanged (1 / 0) + a post; a presented
                                 face / finger / eyes = com.apple.BiometricKit_Sim.<pearl|fingerTouch|oyster>.<match|
                                 nomatch> (default: pearl + fingerTouch, oyster on a Vision Pro)
  gamepad <b> down|up | gamepad press <b> | gamepad stick left|right <x> <y> | gamepad state <16 values>
                                 the simulated game controller (b: up down left right a b x y l1 r1 l2 r2; sticks
                                 -1..1, y up): IOHIDEventCreateGameControllerEvent (extended), Indigo's value order
  pointer move <dx> <dy> | pointer at <nx> <ny> | pointer click | pointer button down|up | pointer scroll <dx> <dy> |
  pointer remove
                                 Vision Pro: its pointer. iPad: a mouse, added at the first pointer command as Apple's
                                 simulator adds one for pointer capture (IndigoHIDMessageToCreateMouseService: Generic-
                                 Desktop Mouse + Pointer, not built in; pointeruid draws iPadOS's pointer); points,
                                 moved in the interface's orientation (and sped up: one +300 went 450 points, 30 x 10
                                 went 305); "at" places it (a pointer event with kIOHIDEventOptionIsAbsolute, 0..1 of the
                                 screen as the interface is turned), exact, used by the launcher's pointer capture
                                 (Apple's capture sends relative moves and hides the Mac's cursor); scroll = one phased
                                 gesture (Began, 3 Changed,
                                 Ended), positive dy scrolls the page down; remove takes the mouse away
  trackpad on|off                iPad: Apple's trackpad device (Simulate Trackpad). Its gestures are not simulated: Apple's
                                 simulator takes them from the Mac's own trackpad events, and none built here (scroll,
                                 scale, rotation, dock-swipe events alone or with finger frames, also through Apple's
                                 Indigo trackpad message) moved anything in Apple's iPad simulator or here
  appswitcher                    iPhone / iPad: the home button pressed twice in quick succession (DeviceHub's App
                                 Switcher; SpringBoard counts the presses itself, also on Face ID models)
  standby show|hide              iPhone StandBy: state of com.apple.springboard.ambient.presentation + a post, as
                                 SimulatorCoreDevicePlugin; SpringBoard presents it when locked and in landscape. Its
                                 blurred widget backdrop comes from PosterBoard (posterboardservices, a non-launching
                                 endpoint: only while PosterBoard runs), i.e. with the "wallpaper" feature
  gesture doubletap|wristflick|<n>
                                 Apple Watch hand gestures: WatchGestures' -[WAGSimulatorServerInterface
                                 handleGestureType:] (Apple's watchOS SimulatorHID does the same; watchgesturesd turns it
                                 into a GenericGesture event): 0 double tap (opens the Smart Stack), 1 wrist flick
```

The launcher turns an iPhone's or iPad's picture with the device orientation (<data>/orientation; landscapeLeft = the
framebuffer shown a quarter counterclockwise, landscapeRight clockwise, portraitUpsideDown upside down; flat keeps the
last turn), as Apple's Simulator turns its window, and maps touches back (hid::unrotate). Its iPad tool bar has a
pointer capture toggle: hover moves the iPad's pointer, a click clicks, the wheel scrolls.

Looked at, not added: Simulate Fall (DeviceHub sets the state of com.apple.sos.user-fell and posts it; states 0-3 sent
to an unpaired watch in Apple's simulator showed nothing); the Siri Remote's swipes (DeviceHub's remote goes through
Xcode's in-device dtuhidd, which synthesizes a whole Siri Remote with UniversalHID "FluidTouch" gesture reports;
Indigo touches with every swipe mask sent to Apple's remote digitizer moved no focus, so the launcher keeps wheel =
arrow keys); Apple's second iPhone digitizer (Built-In 0, another display's UUID: for an external display) and
visionOS's selection stream / manipulator services (registered by Apple, nothing in the simulator drives them).

## Inputs 2: Apple's virtual HID services and buttons (Xcode 27, read 2026-11-15)

Listed with IOHIDEventSystemClientCopyServices in booted Xcode 27 simulators (iPhone 17 Pro iOS 27.2, Apple TV 4K
tvOS 27.2, Watch Series 11 watchOS 27.2, Vision Pro visionOS 27.2; `x86sim spawn <probe>` does the same here), with
SimulatorHID's own service map (SimHIDVirtualServiceManager):

| Apple's service | page/usage | properties | platforms | here |
|---|---|---|---|---|
| MainScreenButtonsService | 0x0b/1 (Telephony) | Built-In, Authenticated; iPhone: PressCountUsagePairs (0xc0040), tracking off; Vision Pro: tracking on, 2 s timeouts | iOS, watchOS, visionOS | buttons_svc (same properties) |
| AppleTVRemote{Buttons,Digitizer,MotionLite}Service | 0x0c/1; 0x0d/5 + 0x0d/0xc; 0xff00/0x10 | VendorID 76, ProductID 614, one PhysicalDeviceUniqueID: "source:externalAccessory" | tvOS | buttons_svc + remote-digitizer + remote-motion (touch surface and motion registered, not driven: Indigo touches sent to Apple's remote digitizer moved no focus in a test) |
| GamePadService "Gamepad" | 1/5 | PhysicalDeviceUniqueID SimulatedGamepad, GameControllerType 1 | iOS, tvOS, visionOS | gamepad_svc |
| MainScreenDigitalCrownService | 0xff0b/1 | built in | watchOS | crown_svc |
| DigitalDialService | 1/0x37 | Built-In | visionOS | dial_svc |
| ProximitySensorService | 0xff00/8 | Built-In | iOS, visionOS | proximity_svc (never driven, as Apple's) |
| ExternalKeyboardService | 1/6 | press counts on iPhone / Vision Pro | all, at boot | keyboard_svc, attached at the first key (Speed 34) |
| ScreenTouchService.<display> | 0x0d/4 | DisplayIntegrated, displayUUID | iOS, watchOS, visionOS | touch_svc (also on tvOS) |
| PalomaPose / SelectionStream / Manipulator | 0xff00/0x6f, 0x71, 0x72 | Built-In | all | paloma_svc on visionOS only (the other two have no observable use) |
| Mouse (pointer capture) | 1/2 + 1/1 | not built in | iPad, on demand | pointer_svc on demand |
| Trackpad ("Simulate Trackpad") | 0x0d/0xc + 0xff00/1, 0xff00/0xc, 1/1, 1/2, 0x0d/5 | not built in | iPad, on demand | trackpad_svc ("trackpad on"); gestures not driven |

The buttons Apple's simulator presses (Indigo button sources -> usages, backboardd's names; the same table on every
platform, what reacts differs):

| name here | usage | backboardd | iPhone | Watch | TV (Siri Remote) | Vision Pro |
|---|---|---|---|---|---|---|
| home | 0x0c/0x40 | Home | home | crown press | closes the app | crown press: Home View |
| lock / power | 0x0c/0x30 | Lock / Power | side button | sleep / wake | power (held: sleep) | |
| side | iPhone 0x0c/0x30, Watch 0x0c/0x95 | Side | = lock | side button: Control Center | | |
| volup / voldown | 0x0c/0xe9 / 0xea | VolumeIncrement / Decrement | volume HUD | | volume | |
| siri | 0x0c/0xcf; TV 0x0c/0x04 | VoiceCommand / Microphone | (no UI on Apple's either) | | Siri / Dictation | |
| action | iPhone 0x0b/0x2d, Watch 0xff01/0x200 | Action | Ring / Silent (hold) | Ultra's action button | | |
| wallet | 0x0c/0x1ba | DigitalWallet | (no destination) | (no destination) | | |
| select | 0x0c/0x80 | Select | | | click | |
| menu | TV: GenericDesktop 0x86; else 0x0c/0x46 | Menu | | | Back, one level | |
| tv / playpause / back / mute | 0x0c/0x60, 0xcd, 0x224, 0xe2 | TV / PlayOrPause / ACBack | | | | |

On an Apple TV the keyboard is the remote (tvOS turns hardware arrow keys into focus moves, return into select and
escape into back); the Siri Remote's own HID services (buttons, touch surface) are not there. Its buttons are consumer
usages on the buttons service, found by trying them in tvOS 27.2: 0x46 (Menu Escape) is Back, one level as the keyboard's
Escape (0x40, Menu, closes the app from any depth; 0x224 AC Back does nothing), 0x60 the TV button (PineBoard's home
from anywhere), 0xcd play / pause, 0xe2 mute. 0x223 (AC Home) also goes back one level. Holding 0x30 (power) puts the TV
to sleep, and nothing here wakes it again; the launcher does not offer it. On an Apple Watch 0x40 is the crown's
press (watch face / app grid) and a short 0x30 the side button (sleep / wake the screen); a crown step of ~120
scrolls a Settings list about one row.
