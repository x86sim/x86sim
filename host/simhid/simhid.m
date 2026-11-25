// x86Sim: stand-in for CoreSimulator's SimulatorHID.framework (arm64 simulator builds), loaded by backboardd.
//
// How the runtime uses it (SimulatorClient.framework, IndigoHIDSystemSpawnLoopback(IOHIDEventSystemRef)):
//   path = getenv("SIMULATOR_HID_SYSTEM_MANAGER"); bundle = [NSBundle bundleWithPath:path];
//   cls = [bundle principalClass]  (Info.plist NSPrincipalClass "SimHIDSystem"), must conform to the protocol
//   SimulatorClientHIDSystemManager; manager = [[cls alloc] initUsingHIDService:<backboardd's IOHIDEventSystemRef>
//   error:&err] and is kept forever.
// Apple's SimHIDSystem receives "Indigo" HID messages from Simulator.app over a mach port and dispatches them
// through virtual HID services it adds to that event system (_IOHIDServiceCreateVirtual + _IOHIDEventSystemAddService
// + _IOHIDServiceDispatchEvent). We do the same with a built-in touchscreen, the main-screen buttons and a keyboard,
// fed by a Unix socket (simview / cli/libexec/simtouch.py on Linux) instead of Indigo messages.
//
// Socket: $SIMHID_SOCKET (default <SIMULATOR_SHARED_RESOURCES_DIRECTORY>/simhid.sock), stream, any number of
// clients, one text command per line (a path too long for sun_path: a symlink to a short one, see listen_thread).
// The commands are in docs/protocols/SIMHID.md; handle_line parses them.
#import <Foundation/Foundation.h>
#import <objc/message.h>
#include <mach-o/dyld.h>
#import <objc/runtime.h>
typedef int IOReturn;
typedef uint32_t IOOptionBits;
#include <dispatch/dispatch.h>
#include <mach/mach_time.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <pthread.h>
#include <unistd.h>
#include <fcntl.h>
#include <pwd.h>
#include <stdio.h>
#include <time.h>

typedef struct __IOHIDEvent* IOHIDEventRef;
typedef struct __IOHIDService* IOHIDServiceRef;
typedef struct __IOHIDEventSystem* IOHIDEventSystemRef_;
typedef double IOHIDFloat;

// IOKit private (IOHIDFamily: IOHIDServicePrivate.h / IOHIDEventSystemPrivate.h). The callback table layout was read
// from the runtime's IOKit (__IOHIDServiceCreateVirtualNoInit copies 0x58 bytes when version == 2; the service calls
// +0x8 open, +0x10 close, +0x18 copyProperty, +0x20 setProperty, +0x28 setEventCallback, +0x30/+0x38 schedule/
// unschedule, +0x40 copyEvent, +0x48 setOutputEvent, +0x50 copyMatchingEvent).
typedef struct {
	uint64_t version;
	Boolean (*open)(void* target, void* refcon, IOOptionBits options);
	void (*close)(void* target, void* refcon, IOOptionBits options);
	CFTypeRef (*copyProperty)(void* target, void* refcon, CFStringRef key);
	Boolean (*setProperty)(void* target, void* refcon, CFStringRef key, CFTypeRef value);
	void (*setEventCallback)(void* target, void* refcon, void* callback, void* cbTarget, void* cbRefcon);
	void (*schedule)(void* target, void* refcon, dispatch_queue_t queue);
	void (*unschedule)(void* target, void* refcon, dispatch_queue_t queue);
	IOHIDEventRef (*copyEvent)(void* target, void* refcon, uint32_t type, IOHIDEventRef matching, IOOptionBits options);
	IOReturn (*setOutputEvent)(void* target, void* refcon, IOHIDEventRef event);
	IOHIDEventRef (*copyMatchingEvent)(void* target, void* refcon, CFDictionaryRef matching);
} VirtualCallbacks;
extern IOHIDServiceRef _IOHIDServiceCreateVirtual(CFAllocatorRef, uint64_t serviceID, const VirtualCallbacks*, void* target, void* refcon);
extern void _IOHIDEventSystemAddService(IOHIDEventSystemRef_, IOHIDServiceRef);
extern void _IOHIDServiceDispatchEvent(IOHIDServiceRef, IOHIDEventRef);
extern IOHIDEventRef IOHIDEventCreateDigitizerEvent(CFAllocatorRef, uint64_t ts, uint32_t transducerType, uint32_t index,
	uint32_t identity, uint32_t eventMask, uint32_t buttonMask, IOHIDFloat x, IOHIDFloat y, IOHIDFloat z,
	IOHIDFloat tipPressure, IOHIDFloat barrelPressure, Boolean range, Boolean touch, IOOptionBits options);
extern IOHIDEventRef IOHIDEventCreateDigitizerFingerEvent(CFAllocatorRef, uint64_t ts, uint32_t index, uint32_t identity,
	uint32_t eventMask, IOHIDFloat x, IOHIDFloat y, IOHIDFloat z, IOHIDFloat tipPressure, IOHIDFloat twist,
	Boolean range, Boolean touch, IOOptionBits options);
extern IOHIDEventRef IOHIDEventCreateKeyboardEvent(CFAllocatorRef, uint64_t ts, uint32_t usagePage, uint32_t usage,
	Boolean down, IOOptionBits flags);
extern void IOHIDEventAppendEvent(IOHIDEventRef, IOHIDEventRef, IOOptionBits);
extern void IOHIDEventSetIntegerValue(IOHIDEventRef, uint32_t field, CFIndex value);
// Models 2: kIOHIDEventTypeHingeAngle (44): fields 1 AngleDegrees, 2 MechanicalAngleDegrees, 3 velocity (deg/s),
// 4 state (0 unknown, 1 closed, 2 open)
extern IOHIDEventRef IOHIDEventCreateHingeAngleEvent(CFAllocatorRef, uint64_t ts, IOOptionBits, double angle, double mechanical, double velocity);
#define kIOHIDEventTypeHingeAngle 44
#define kFieldHingeState ((44 << 16) | 4)
// CoreMotion's CLAngleNotifier drops a hinge event whose phase is 0 ("copied event is invalid")
extern void IOHIDEventSetPhase(IOHIDEventRef, uint16_t phase);
// Platforms 1: the Digital Crown's events (Apple's watchOS SimulatorHID decodes the host's crown messages into these)
extern IOHIDEventRef IOHIDEventCreateScrollEvent(CFAllocatorRef, uint64_t ts, IOHIDFloat x, IOHIDFloat y, IOHIDFloat z,
	IOOptionBits options);
extern IOHIDEventRef IOHIDEventCreateVendorDefinedEvent(CFAllocatorRef, uint64_t ts, uint32_t page, uint32_t usage, uint32_t version,
	const uint8_t* data, CFIndex length, IOOptionBits options);
extern IOHIDEventRef IOHIDEventCreateButtonEvent(CFAllocatorRef, uint64_t ts, uint32_t buttonMask, IOHIDFloat pressure, IOOptionBits options);
extern CFDataRef IOCFSerialize(CFTypeRef object, CFOptionFlags options);
// Inputs 2: Apple's SimulatorHID turns the host's game controller state (IndigoHIDMessageForGameControllerEvent: 16 doubles
// 0..1 / -1..1) into this, IOFixed (16.16) values in this order, controller type 1 (extended gamepad)
extern IOHIDEventRef IOHIDEventCreateGameControllerEvent(CFAllocatorRef, uint64_t ts, uint32_t type, int32_t up, int32_t down,
	int32_t left, int32_t right, int32_t x, int32_t y, int32_t a, int32_t b, int32_t lx, int32_t ly, int32_t rx, int32_t ry,
	int32_t l1, int32_t l2, int32_t r1, int32_t r2, IOOptionBits options);
#include <notify.h>
#include <dlfcn.h>
#include <math.h>

#define kDigitizerTransducerHand 3
#define kDigitizerRange 0x1
#define kDigitizerTouch 0x2
#define kDigitizerPosition 0x4
#define kDigitizerIdentity 0x20
#define kFieldDigitizerIsDisplayIntegrated ((11 << 16) | 25)
// x86Sim: how Apple's Simulator marks edge touches (SimulatorKit IndigoHIDMessageForMouseNSEvent, Xcode 27.1): the
// IndigoHIDEdge (0 none, 1 top, 2 left, 3 bottom, 4 right) picks the digitizer event mask from a table, 0x40000 |
// {0, 0x02000000, 0x08000000, 0x01000000, 0x04000000} (the swipe direction away from that edge: top = swipe down,
// bottom = swipe up ...), and SimulatorHID passes that mask to IOHIDEventCreateDigitizerFingerEvent on every phase.
// SpringBoard's system gestures (home indicator, Control/Notification Center) only claim touches carrying these bits.
// A touch from no edge carries none of them: Apple's iOS 27.2 simulator delivers a mid-screen press with event mask
// Range|Touch only (the app's UIEvent _hidEvent, checks/P16_Touch.swift). SwipeLocked (0x40000) on it made UIKit's
// UITouch _edgeType 4, so a navigation controller's edge-swipe pan never failed on a still finger and blocked the
// long press behind it: a long press in the Files browser (or any pushed collection view) ended as a tap.
static const uint32_t edge_mask[5] = { 0, 0x02040000, 0x08040000, 0x01040000, 0x04040000 };
static int touch_edge;

// x86Sim arm64: also mirror to a host-visible log file. stderr under the JIT can be late or
// interleaved with backboardd's own logging; a dedicated file lets us see whether the socket
// thread actually runs handle_line / dispatch.
static FILE* g_logf;
static void slog(const char* fmt, ...) {
	va_list ap; va_start(ap, fmt);
	fprintf(stderr, "simhid: "); vfprintf(stderr, fmt, ap); fprintf(stderr, "\n");
	if (!g_logf) {
		const char* p = getenv("SIMHID_LOG");
		char def[512];
		if (!p || !*p) {  // Linux user's scratch, seen through Darling's SystemRoot (same user name)
			struct passwd* pw = getpwuid(getuid());
			snprintf(def, sizeof def, "/Volumes/SystemRoot/home/%s/scratch/x86sim/simhid.log", pw ? pw->pw_name : "");
			p = def;
		}
		g_logf = fopen(p, "a");
		if (g_logf) setvbuf(g_logf, NULL, _IOLBF, 0);
	}
	if (g_logf) {
		// wall-clock ns (same clock as the frame ring's slot t_ns) so touches line up with frames (simrecord.py)
		struct timespec ts;
		clock_gettime(CLOCK_REALTIME, &ts);
		fprintf(g_logf, "%llu simhid: ", (unsigned long long)ts.tv_sec * 1000000000ull + ts.tv_nsec);
		va_list ap2; va_start(ap2, fmt);
		vfprintf(g_logf, fmt, ap2);
		va_end(ap2);
		fprintf(g_logf, "\n");
		fflush(g_logf);
	}
	va_end(ap);
}

typedef struct {
	const char* name;
	uint64_t id;
	CFDictionaryRef props;
	IOHIDServiceRef service;
	dispatch_queue_t queue;
} VService;

static VService touch_svc, buttons_svc, keyboard_svc;
// Models 2: a device with two integrated displays (capabilities) gets a touchscreen per extra display and a hinge
static VService inner_touch_svc, hinge_svc, avp_svc;
static uint32_t inner_display_id;
static VService* cur_touch = &touch_svc;   // the touchscreen of the command being handled (@<screenID> prefix)
// Platforms 1: an Apple Watch's Digital Crown, as Apple's watchOS SimulatorHID registers it
// ("com.apple.SimulatorHID.MainScreenDigitalCrownService": Apple vendor usage page 0xff0b, usage 1, built in)
static VService crown_svc;
// tvOS: the simulator's virtual game controller. Apple's tvOS SimulatorHID (SimHIDVirtualServiceManager) adds
// "com.apple.SimulatorHID.GamePadService" at start, connected, with exactly these properties (no Transport / VendorID):
// gamecontrollerd matches GenericDesktop / GamePad and lists it as an MFi "Gamepad" (extended gamepad, 23 buttons,
// 6 axes, 3 dpads), so GCController.controllers() has 1 controller in Apple's tvOS simulator with no input sent.
static VService gamepad_svc;
// Platforms 2: visionOS's pointer. RealitySimulation's RSPointerInputProvider takes mouse events (GenericDesktop /
// Mouse) from a trackpad, Universal Control or a service whose Transport is "Virtual-AssistiveTouch", and turns them
// into the system pointer (hover, and a click is a tap where it points). The simulator's touchscreen is not an input
// on visionOS. Commands: pointer move <dx> <dy> | pointer button down|up | pointer click
static VService pointer_svc;
static uint32_t pointer_buttons;
// Inputs 2: the rest of Apple's virtual services (read with a probe of IOHIDEventSystemClientCopyServices in Xcode 27's
// simulators, 2026-11-15; SimHIDVirtualServiceManager): the Apple TV's Siri Remote (buttons, touch surface, motion: Apple's
// Bluetooth vendor 76, product 614, one PhysicalDeviceUniqueID; button presses are "source:externalAccessory" to
// backboardd), the proximity sensor (iPhone, Vision Pro; never driven), the Vision Pro's Digital Crown (a Dial).
static VService remote_digitizer_svc, remote_motion_svc, proximity_svc, dial_svc;
static int platform;   // 0 iOS / iPadOS, 1 tvOS, 2 watchOS, 3 visionOS (SIMULATOR_MODEL_IDENTIFIER)
enum { P_IOS, P_TV, P_WATCH, P_XR };
// Platforms 2: visionOS's natural input as Xcode's simulator sends it (VisionDeviceKitExtension
// IndigoHIDMessageForPalomaCollection -> SimulatorHID __IOHIDEventFromIndigoHIDData): a vendor-defined "collection"
// (page 0xff00, usage 0x6f, version 0xd) with a child per hand (usage 0x70; payload {pinching, chirality 2 left /
// 1 right, touching}; a translation and a quaternion orientation) and a gaze child (usage 0x71; origin as a translation,
// direction as a rotation event). RealitySimulation's RSVirtualHMDInput turns it into the eyes' pose and the hands.
static VService paloma_svc;
static void dispatch_event(VService* s, IOHIDEventRef e);
extern IOHIDEventRef IOHIDEventCreateTranslationEvent(CFAllocatorRef, uint64_t ts, IOHIDFloat x, IOHIDFloat y, IOHIDFloat z, IOOptionBits options);
extern IOHIDEventRef IOHIDEventCreateRotationEvent(CFAllocatorRef, uint64_t ts, IOHIDFloat x, IOHIDFloat y, IOHIDFloat z, IOOptionBits options);
extern IOHIDEventRef IOHIDEventCreateQuaternionOrientationEvent(CFAllocatorRef, uint64_t ts, IOHIDFloat w, IOHIDFloat x, IOHIDFloat y,
	IOHIDFloat z, IOOptionBits options);
extern void IOHIDEventAppendEvent(IOHIDEventRef, IOHIDEventRef, IOOptionBits);
extern void IOHIDEventSetPhase(IOHIDEventRef, uint16_t);
static IOHIDEventRef paloma_hand(uint64_t ts, int left, int pinch, int touch, uint16_t phase, const float t[3], const float q[4]) {
	uint8_t data[3] = { (uint8_t)(pinch != 0), (uint8_t)(left ? 2 : 1), (uint8_t)(touch != 0) };
	IOHIDEventRef h = IOHIDEventCreateVendorDefinedEvent(kCFAllocatorDefault, ts, 0xff00, 0x70, 0xd, data, 3, 2);
	IOHIDEventSetPhase(h, phase);
	IOHIDEventRef tr = IOHIDEventCreateTranslationEvent(kCFAllocatorDefault, ts, t[0], t[1], t[2], 0);
	IOHIDEventRef qo = IOHIDEventCreateQuaternionOrientationEvent(kCFAllocatorDefault, ts, q[3], q[0], q[1], q[2], 0);
	IOHIDEventAppendEvent(h, tr, 0); IOHIDEventAppendEvent(h, qo, 0);
	CFRelease(tr); CFRelease(qo);
	return h;
}
// hands: 0 none (a gaze-only "selection"), else both hands with these pinch states
static void paloma_send(const float o[3], const float d[3], int hands, int pinchL, int pinchR, uint16_t phaseL, uint16_t phaseR) {
	uint64_t ts = mach_absolute_time();
	IOHIDEventRef top = IOHIDEventCreateVendorDefinedEvent(kCFAllocatorDefault, ts, 0xff00, 0x6f, 0xd, NULL, 0, 2);
	IOHIDEventSetPhase(top, 2);
	if (hands) {
		static const float tl[3] = { -0.15f, -0.35f, -0.35f }, tr[3] = { 0.15f, -0.35f, -0.35f }, qi[4] = { 0, 0, 0, 1 };
		IOHIDEventRef l = paloma_hand(ts, 1, pinchL, 0, phaseL, tl, qi), r = paloma_hand(ts, 0, pinchR, 0, phaseR, tr, qi);
		IOHIDEventAppendEvent(top, l, 0); IOHIDEventAppendEvent(top, r, 0);
		CFRelease(l); CFRelease(r);
	}
	IOHIDEventRef g = IOHIDEventCreateVendorDefinedEvent(kCFAllocatorDefault, ts, 0xff00, 0x71, 0xd, NULL, 0, 2);
	IOHIDEventRef go = IOHIDEventCreateTranslationEvent(kCFAllocatorDefault, ts, o[0], o[1], o[2], 1);
	IOHIDEventRef gd = IOHIDEventCreateRotationEvent(kCFAllocatorDefault, ts, d[0], d[1], d[2], 1);
	IOHIDEventAppendEvent(g, go, 0); IOHIDEventAppendEvent(g, gd, 0);
	CFRelease(go); CFRelease(gd);
	IOHIDEventAppendEvent(top, g, 0);
	CFRelease(g);
	dispatch_event(&paloma_svc, top);
}
// The display's projection (RealitySimulation's -[RSFrameDescription getProjectionMatrix:forEye:], eye 2: the view the
// simulator shows), captured from each frame, turns a point on the display into a gaze direction in head space.
static float proj[16];
static volatile int have_proj;
static IMP hmd_update_orig;
static void hmd_update(id self, SEL sel, id headset, id desc) {
	if (desc) {
		float m[16];
		((void (*)(id, SEL, void*, long long)) objc_msgSend)(desc, sel_registerName("getProjectionMatrix:forEye:"), m, 2);
		if (m[0] != 0 && m[5] != 0) { memcpy(proj, m, sizeof m); have_proj = 1; }
	}
	((void (*)(id, SEL, id, id)) hmd_update_orig)(self, sel, headset, desc);
}
static void hmd_hook(const struct mach_header* mh, intptr_t slide) {
	(void) mh; (void) slide;
	static int done;
	Class c = done ? Nil : objc_getClass("RSVirtualHMDInput");
	Method m = c ? class_getInstanceMethod(c, sel_registerName("headsetDidUpdate:withFrameDescription:")) : NULL;
	if (!m) return;
	done = 1;
	hmd_update_orig = method_setImplementation(m, (IMP) hmd_update);
	slog("hooked RSVirtualHMDInput headsetDidUpdate: (gaze from display points)");
}
// x, y normalized on the display (0..1, origin top left) -> head-space direction
static void display_dir(double x, double y, float d[3]) {
	double nx = 2 * x - 1, ny = 1 - 2 * y;
	if (have_proj) {
		d[0] = (float)((nx + proj[8]) / proj[0]);
		d[1] = (float)((ny + proj[9]) / proj[5]);
	} else {   // measured once: about 67 by 45 degrees
		d[0] = (float)(nx * 0.667); d[1] = (float)(ny * 0.41);
	}
	d[2] = -1;
}
// the right hand pinching (or just released) at its rest position moved by (dx, dy) metres
static void paloma_send_hand(const float o[3], const float d[3], int pinch, float dx, float dy) {
	uint64_t ts = mach_absolute_time();
	IOHIDEventRef top = IOHIDEventCreateVendorDefinedEvent(kCFAllocatorDefault, ts, 0xff00, 0x6f, 0xd, NULL, 0, 2);
	IOHIDEventSetPhase(top, 2);
	static const float tl[3] = { -0.15f, -0.35f, -0.35f }, qi[4] = { 0, 0, 0, 1 };
	float tr[3] = { 0.15f + dx, -0.35f + dy, -0.35f };
	IOHIDEventRef l = paloma_hand(ts, 1, 0, 0, 0, tl, qi), r = paloma_hand(ts, 0, pinch, 0, 0, tr, qi);
	IOHIDEventAppendEvent(top, l, 0); IOHIDEventAppendEvent(top, r, 0);
	CFRelease(l); CFRelease(r);
	IOHIDEventRef g = IOHIDEventCreateVendorDefinedEvent(kCFAllocatorDefault, ts, 0xff00, 0x71, 0xd, NULL, 0, 2);
	IOHIDEventRef go = IOHIDEventCreateTranslationEvent(kCFAllocatorDefault, ts, o[0], o[1], o[2], 1);
	IOHIDEventRef gd = IOHIDEventCreateRotationEvent(kCFAllocatorDefault, ts, d[0], d[1], d[2], 1);
	IOHIDEventAppendEvent(g, go, 0); IOHIDEventAppendEvent(g, gd, 0);
	CFRelease(go); CFRelease(gd);
	IOHIDEventAppendEvent(top, g, 0);
	CFRelease(g);
	dispatch_event(&paloma_svc, top);
}
extern IOHIDEventRef IOHIDEventCreateRelativePointerEvent(CFAllocatorRef, uint64_t ts, IOHIDFloat x, IOHIDFloat y, IOHIDFloat z,
	uint32_t buttonMask, uint32_t oldButtonMask, IOOptionBits options);

static double hinge_deg, hinge_vel;
static uint16_t hinge_phase = 2;
// Models 2: CoreMotion's device state (the magic pose SpringBoard's SBFDevicePoseProvider combines with the angle:
// "hinge .unknown" without it, and no display swap). On a device it is the SPU's {0xff0c, 0x22} report; the simulator
// build of CMDeviceStateManager also observes the distributed notification CMDeviceStateEventSendNotification with
// NSNumber properties A, A0, A1 (device orientation and that of each half, 0..6), B (hinge state, 0..5), C (tabletop
// pose, 0..11), D (stable orientation state, 0..4); -[CMMagicPoseManager createMagicPoseEventFromDeviceStateEvent:]
// copies them 1:1 into a CMMagicPoseEvent. Hinge state from the strings: 1 Closed, 2 PartiallyOpen, 3 OpenFlat.
// the angle for the launcher (which screen to show): <SIMULATOR_SHARED_RESOURCES_DIRECTORY>/hinge, degrees
static int cur_orientation(void);
static void write_hinge_file(double deg) {
	const char* d = getenv("SIMULATOR_SHARED_RESOURCES_DIRECTORY");
	if (!d) return;
	char p[1100], t[1110];
	snprintf(p, sizeof p, "%s/hinge", d);
	snprintf(t, sizeof t, "%s.tmp", p);
	FILE* f = fopen(t, "w");
	if (!f) return;
	fprintf(f, "%.1f\n", deg);
	fclose(f);
	rename(t, p);
}
static void post_device_state(double deg) {
	write_hinge_file(deg);
	long hs = deg <= 5 ? 1 : deg >= 170 ? 3 : 2;
	Class c = NSClassFromString(@"NSDistributedNotificationCenter");
	id center = c ? ((id (*)(id, SEL))objc_msgSend)((id)c, sel_registerName("defaultCenter")) : nil;
	if (!center) { slog("hinge: no NSDistributedNotificationCenter"); return; }
	int o = cur_orientation();
	if (o < 1 || o > 6) o = 1;
	NSDictionary* info = @{ @"CMDeviceStateEventPropertyAKey": @(o), @"CMDeviceStateEventPropertyA0Key": @(o),
		@"CMDeviceStateEventPropertyA1Key": @(o), @"CMDeviceStateEventPropertyBKey": @((float)deg),
		@"CMDeviceStateEventPropertyCKey": @0, @"CMDeviceStateEventPropertyDKey": @2 };
	((void (*)(id, SEL, id, id, id, BOOL))objc_msgSend)(center, sel_registerName("postNotificationName:object:userInfo:deliverImmediately:"),
		@"CMDeviceStateEventSendNotification", nil, info, YES);
	slog("hinge: device state B=%ld posted", hs);
}
static IOHIDEventRef hinge_event(void) {
	IOHIDEventRef e = IOHIDEventCreateHingeAngleEvent(kCFAllocatorDefault, mach_absolute_time(), 0, hinge_deg, hinge_deg, hinge_vel);
	if (e) {
		IOHIDEventSetIntegerValue(e, kFieldHingeState, hinge_deg <= 5 ? 1 : 2);
		IOHIDEventSetPhase(e, hinge_phase);
		// CLAngleNotifier takes CMAngle.progress (the fold progress SpringBoard's pose uses, "fold:") from a vendor child
		// {0xff0c, 0x80} of 21 bytes: u64 gesture-began and u64 message-sent continuous timestamps, float progress,
		// flags (bit 0: wake event); without it progress is 0 and the pose stays fully closed
		struct __attribute__((packed)) { uint64_t began, sent; float progress; uint8_t flags; } d = {
			mach_continuous_time(), mach_continuous_time(), (float)(hinge_deg / 180.0), 0 };
		IOHIDEventRef c = IOHIDEventCreateVendorDefinedEvent(kCFAllocatorDefault, mach_absolute_time(), 0xff0c, 0x80, 0,
			(const uint8_t*)&d, sizeof(d), 0);
		if (c) { IOHIDEventAppendEvent(e, c, 0); CFRelease(c); }
	}
	return e;
}
// Models 2: a hinge sensor reports all the time (the SPU at 100 Hz), not only while the hinge moves: SpringBoard
// keeps a partly open phone's fold state (the outer screen's glass blur while it is open part way) only while
// reports arrive, and dropped it about a second after the last one (holding the launcher's slider still at 30
// degrees cleared the blur; re-sending the same angle every 50 ms kept it). So between moves the angle is reported
// again every 100 ms while the phone is part open (closed or flat it rests: nothing depends on it there). Measured
// at 30 degrees: every 100 ms (and 50) keeps the blur, every 150 ms or slower loses it; backboardd then spends
// ~70% of a core and SpringBoard ~20% drawing the live blur, the same at 50 and 100 ms (the reports themselves cost
// nothing measurable: 1-5% at 150-800 ms, where the blur is gone).
static volatile int hinge_moving;
static volatile int hinge_stream_ms = 100;   // "hingestream <ms>": the interval (0: off)
static void avp_send_angle(double deg);
static IOHIDEventRef hinge_event(void);
static pthread_mutex_t cmd_mu;
static void* hinge_stream_thread(void* arg) {
	for (;;) {
		int ms = hinge_stream_ms;
		usleep((ms > 0 ? ms : 250) * 1000);
		pthread_mutex_lock(&cmd_mu);
		if (ms > 0 && hinge_svc.service && !hinge_moving && hinge_deg > 0.5 && hinge_deg < 179.5) {
			hinge_vel = 0;
			hinge_phase = 2;
			dispatch_event(&hinge_svc, hinge_event());
			avp_send_angle(hinge_deg);
		}
		pthread_mutex_unlock(&cmd_mu);
	}
	return NULL;
}
// Models 2: the hinge as Apple's Simulator gives it. The Duo's runtime (24B91016s) has locationd's
// CMDeviceStateRelayManager: its CLAVPMonitor matches a service {0xff61, 0x5b} and takes vendor-defined events from it
// whose data is an IOCFSerialize'd dictionary (at most 256 bytes) {source, type, value}: "hinge-slider-control" /
// "range" / angle in degrees, "orientation-picker-control" / "enum" / "portrait" (also landscape-left, landscape-right,
// faceup, facedown, ambiguous); a button event (type 3) carries the standby state in field (3 << 16) | 2. Once it has
// angle, orientation and standby, the relay interpolates the angle and publishes it on its own virtual services
// "CoreMotion_DeviceStateRelay_Crane" {0xff0c, 0x21} and "_DeviceState" {0xff0c, 0x22}. CoreMotion's angle notifier
// in SpringBoard then reads the relay and ignored hinge_svc: with locationd running (feature "location") the fold
// went nowhere. hinge_svc stays for boots without locationd, where it is the only hinge.
static void avp_send(NSDictionary* d) {
	if (!avp_svc.service) return;
	CFDataRef data = IOCFSerialize((CFTypeRef)d, 0);
	if (!data) { slog("avp: IOCFSerialize failed"); return; }
	IOHIDEventRef e = IOHIDEventCreateVendorDefinedEvent(kCFAllocatorDefault, mach_absolute_time(), 0xff61, 0x5b, 0,
		CFDataGetBytePtr(data), CFDataGetLength(data), 0);
	CFRelease(data);
	dispatch_event(&avp_svc, e);
}
// whole degrees: IOCFSerialize takes integer CFNumbers only (a fractional angle failed to serialize and was dropped,
// so a move whose steps were not whole degrees, 0 -> 90 in 1.5 degree steps, reached the relay half the time)
static void avp_send_angle(double deg) {
	avp_send(@{ @"source": @"hinge-slider-control", @"type": @"range", @"value": @((long)lround(deg)) });
}
// the device orientation as the relay's orientation picker names it (Simulator's values; portraitUpsideDown has none)
static NSString* avp_orientation_name(void) {
	switch (cur_orientation()) {
	case 3: return @"landscape-left";
	case 4: return @"landscape-right";
	case 5: return @"faceup";
	case 6: return @"facedown";
	default: return @"portrait";
	}
}
static void avp_send_orientation(void) {
	avp_send(@{ @"source": @"orientation-picker-control", @"type": @"enum", @"value": avp_orientation_name() });
}
// the inputs besides the angle the relay waits for: the device orientation, not in standby
static void avp_send_pose(void) {
	if (!avp_svc.service) return;
	IOHIDEventRef b = IOHIDEventCreateButtonEvent(kCFAllocatorDefault, mach_absolute_time(), 0, 0, 0);
	if (b) { IOHIDEventSetIntegerValue(b, (3 << 16) | 2, 0); dispatch_event(&avp_svc, b); }
	avp_send_orientation();
}
static IOHIDEventSystemRef_ hid_system;

static Boolean v_open(void* t, void* r, IOOptionBits o) {
	// Speed 34: NULL-safe: after the idle detach (_IOHIDServiceTerminate) IOKit calls close / unschedule with a NULL
	// target (backboardd crashed in v_close on the first gate)
	slog("v_open %s", t ? ((VService*)t)->name : "(null)");
	return true;
}
static void v_close(void* t, void* r, IOOptionBits o) { slog("v_close %s", t ? ((VService*)t)->name : "(null)"); }
static CFTypeRef v_copyProperty(void* t, void* r, CFStringRef key) {
	VService* s = t;
	if (!s || !s->props) return NULL;
	CFTypeRef v = CFDictionaryGetValue(s->props, key);
	return v ? CFRetain(v) : NULL;
}
static VService trackpad_svc;
static Boolean v_setProperty(void* t, void* r, CFStringRef key, CFTypeRef value) {
	// keyboard: logged (timestamped) to find when the hardware-keyboard session is up, see the settle in handle_line
	if (t == &keyboard_svc && g_logf && key) {
		char k[96] = ""; CFStringGetCString(key, k, sizeof k, kCFStringEncodingUTF8);
		slog("v_setProperty keyboard %s", k);
	}
	// Inputs 2: an iPad's mouse / trackpad keeps what backboardd sets (pointer acceleration, tracking speed ...) and
	// answers it back, as Apple's SimHIDVirtualServicePropertyProvider does for every service
	VService* s = t;
	if (s && key && value && (s == &pointer_svc || s == &trackpad_svc) && s->props) {
		CFMutableDictionaryRef m = CFDictionaryCreateMutableCopy(kCFAllocatorDefault, 0, s->props);
		CFDictionarySetValue(m, key, value);
		s->props = m;   // (the old one is not released: copyProperty may be reading it on another thread)
		slog("v_setProperty %s %s = %s", s->name, [(__bridge NSString*)key UTF8String], [[(id)value description] UTF8String]);
	}
	return true;
}
static void v_setEventCallback(void* t, void* r, void* cb, void* a, void* b) {
	slog("v_setEventCallback %s cb=%p", t ? ((VService*)t)->name : "(null)", cb);
}
static void v_schedule(void* t, void* r, dispatch_queue_t q) {
	VService* s = t;
	if (!s) return;
	if (q) dispatch_retain(q);
	s->queue = q;
	slog("v_schedule %s queue=%p", s->name, q);
}
static void v_unschedule(void* t, void* r, dispatch_queue_t q) {
	if (!t) { slog("v_unschedule (null)"); return; }
	slog("v_unschedule %s", ((VService*)t)->name);
	((VService*)t)->queue = NULL;
}
static IOHIDEventRef v_copyEvent(void* t, void* r, uint32_t type, IOHIDEventRef m, IOOptionBits o) {
	if (t == &hinge_svc && (type == kIOHIDEventTypeHingeAngle || type == 0)) { slog("hinge copyEvent %.1f", hinge_deg); return hinge_event(); }
	return NULL;
}
static IOReturn v_setOutputEvent(void* t, void* r, IOHIDEventRef e) { return 0; }
static IOHIDEventRef v_copyMatchingEvent(void* t, void* r, CFDictionaryRef m) {
	if (t == &hinge_svc) { slog("hinge copyMatchingEvent %.1f", hinge_deg); return hinge_event(); }
	return NULL;
}

static const VirtualCallbacks callbacks = { 2, v_open, v_close, v_copyProperty, v_setProperty, v_setEventCallback,
	v_schedule, v_unschedule, v_copyEvent, v_setOutputEvent, v_copyMatchingEvent };

static CFDictionaryRef make_props(NSString* product, int page, int usage, NSDictionary* extra) {
	NSMutableDictionary* d = [NSMutableDictionary dictionaryWithDictionary:@{
		@"PrimaryUsagePage": @(page), @"PrimaryUsage": @(usage),
		@"DeviceUsagePairs": @[ @{ @"DeviceUsagePage": @(page), @"DeviceUsage": @(usage) } ],
		@"Product": product, @"Transport": @"Virtual", @"Built-In": @YES, @"Authenticated": @YES,
		@"VendorID": @0x5ac, @"ProductID": @0, @"LocationID": @0 }];
	if (extra) [d addEntriesFromDictionary:extra];
	return (CFDictionaryRef)[d retain];
}

static void add_service(IOHIDEventSystemRef_ sys, VService* s) {
	s->service = _IOHIDServiceCreateVirtual(kCFAllocatorDefault, s->id, &callbacks, s, NULL);
	if (!s->service) { slog("could not create %s", s->name); return; }
	_IOHIDEventSystemAddService(sys, s->service);
	slog("added %s service (id 0x%llx)", s->name, (unsigned long long)s->id);
}

static uint64_t now_ns(void) { struct timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts); return ts.tv_sec * 1000000000ull + ts.tv_nsec; }

// Speed 34: detach the keyboard service again after SIMHID_KB_IDLE_S (default 30; 0 = never) without a key, as a
// hardware keyboard that goes away. While one is attached UIKit runs in hardware-keyboard mode, and SpringBoard's
// focus system then walks the focus environment on every app transition: +80-100 ms of SpringBoard CPU per warm
// Settings tap (630-720 ms vs 530-550 fresh), which is what "the aged system" was (the gate types into Spotlight /
// Settings search, and the attach stayed for the rest of the boot). The next key attaches it again (with the settle).
extern void _IOHIDEventSystemRemoveService(IOHIDEventSystemRef_, IOHIDServiceRef);
extern void _IOHIDServiceTerminate(IOHIDServiceRef);
static uint64_t kb_last_ns;
static uint64_t kb_idle_ns;  // idle detach of the client that sent the last key (kbidle, else SIMHID_KB_IDLE_S)
static uint64_t kb_default_idle_ns;
static int kb_down;  // keys held (no detach while one is down)
// x86Sim arm64: handle_line runs under cmd_mu (one command at a time, from every client's thread) instead of
// dispatch_sync onto a queue: dispatch_sync from the socket pthread was observed to consume socket bytes without ever
// running the block under the A64 JIT (no touch logs, no UI). Mutex + direct call stays on this GuestThread.
static pthread_mutex_t cmd_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t kb_cv = PTHREAD_COND_INITIALIZER;
// Sleeps on kb_cv while no keyboard is attached (or it never detaches) instead of polling every 2 s for the whole
// boot: a thread that woke up while backboardd was being torn down at phone shutdown called through a stub that no
// longer pointed at code (guest pc garbage right after its sleep(2), backboardd crash reports at the end of boots).
// Woken by a key under cmd_mu. While a keyboard is attached it polls as before (pthread_cond_timedwait never timed
// out here: the keyboard stayed attached).
static void* kb_idle_thread(void* arg) {
	pthread_mutex_lock(&cmd_mu);
	for (;;) {
		const uint64_t idle = kb_idle_ns ? kb_idle_ns : kb_default_idle_ns;   // ~0: never
		if (!keyboard_svc.service || !hid_system || idle == ~0ull) {
			pthread_cond_wait(&kb_cv, &cmd_mu);
			continue;
		}
		pthread_mutex_unlock(&cmd_mu);
		sleep(2);
		pthread_mutex_lock(&cmd_mu);
		// a key-down whose key-up never came (viewer lost focus with a key held) must not keep the keyboard attached
		// forever (+90 ms per tap): held keys count only for 10 minutes after the last key
		const uint64_t since = now_ns() - kb_last_ns;
		if (!keyboard_svc.service || kb_idle_ns == ~0ull || !((kb_down <= 0 || since > 600000000000ull) && since > idle))
			continue;
		IOHIDServiceRef sv = keyboard_svc.service;
		keyboard_svc.service = NULL;
		_IOHIDEventSystemRemoveService(hid_system, sv);
		_IOHIDServiceTerminate(sv);
		CFRelease(sv);
		kb_down = 0;
		slog("keyboard detached (idle)");
	}
	return NULL;
}

static void dispatch_event(VService* s, IOHIDEventRef e) {
	if (!s->service || !e) {
		slog("dispatch_event drop service=%p e=%p", s ? s->service : NULL, e);
		if (e) CFRelease(e);
		return;
	}
	// x86Sim arm64: call _IOHIDServiceDispatchEvent on this thread. dispatch_async onto the
	// service queue / main queue was a silent no-op under the JIT for socket-thread producers
	// (reads happened, UI never saw touches). Direct dispatch matches Indigo's in-thread path.
	_IOHIDServiceDispatchEvent(s->service, e);
	CFRelease(e);
}

// --- events

static bool touching;
static uint64_t touch_up_ns;  // last touch lift (focus guard for keys, see handle_line)
static uint32_t touch_identity = 2;

static void send_touch(const char* phase, double x, double y, int edge) {
	bool down = !strcmp(phase, "down"), up = !strcmp(phase, "up"), move = !strcmp(phase, "move");
	if (!down && !up && !move) return;
	if (move && !touching) return;
	if (down) { if (touching) send_touch("up", x, y, 0); touching = true; touch_identity++; touch_edge = edge; }
	// (a single-finger down while two fingers are down: the two-finger contact is lifted first, see send_touch2)
	if (up) { if (!touching) return; touching = false; touch_up_ns = now_ns(); }
	uint64_t ts = mach_absolute_time();
	// Apple's SimulatorHID passes the same mask on every phase: moves keep the contact bits too (Prathamesh913, #1)
	uint32_t mask = kDigitizerPosition | kDigitizerTouch | kDigitizerRange | kDigitizerIdentity;
	mask |= edge_mask[touch_edge];
	bool in = !up;
	IOHIDEventRef hand = IOHIDEventCreateDigitizerEvent(kCFAllocatorDefault, ts, kDigitizerTransducerHand, 0, 0, mask, 0,
		x, y, 0, in ? 1 : 0, 0, in, in, 0);
	IOHIDEventSetIntegerValue(hand, kFieldDigitizerIsDisplayIntegrated, 1);
	IOHIDEventRef finger = IOHIDEventCreateDigitizerFingerEvent(kCFAllocatorDefault, ts, 1, touch_identity, mask,
		x, y, 0, in ? 1 : 0, 0, in, in, 0);
	IOHIDEventSetIntegerValue(finger, kFieldDigitizerIsDisplayIntegrated, 1);
	IOHIDEventAppendEvent(hand, finger, 0);
	CFRelease(finger);
	slog("touch %s (%.4f,%.4f) edge=%d svc=%s %p queue=%p", phase, x, y, touch_edge, cur_touch->name, cur_touch->service, cur_touch->queue);
	dispatch_event(cur_touch, hand);
}

// x86Sim: two fingers in one hand event, as a multi-touch digitizer reports them (the hand carries the centroid; a
// finger child per contact with its own index and identity; range/touch/identity bits on every event).
static bool touching2;
static uint32_t touch2_identity = 1000;
static void send_touch2(const char* phase, double x1, double y1, double x2, double y2) {
	bool down = !strcmp(phase, "down"), up = !strcmp(phase, "up"), move = !strcmp(phase, "move");
	if (!down && !up && !move) return;
	if (move && !touching2) return;
	if (down) {
		if (touching) send_touch("up", x1, y1, 0);
		if (touching2) send_touch2("up", x1, y1, x2, y2);
		touching2 = true; touch2_identity += 2;
	}
	if (up) { if (!touching2) return; touching2 = false; }
	uint64_t ts = mach_absolute_time();
	// Apple's SimulatorHID passes the same mask on every phase: moves keep the contact bits too (Prathamesh913, #1)
	uint32_t mask = kDigitizerPosition | kDigitizerTouch | kDigitizerRange | kDigitizerIdentity;
	mask |= edge_mask[0];
	bool in = !up;
	IOHIDEventRef hand = IOHIDEventCreateDigitizerEvent(kCFAllocatorDefault, ts, kDigitizerTransducerHand, 0, 0, mask, 0,
		(x1 + x2) / 2, (y1 + y2) / 2, 0, in ? 1 : 0, 0, in, in, 0);
	IOHIDEventSetIntegerValue(hand, kFieldDigitizerIsDisplayIntegrated, 1);
	double xs[2] = { x1, x2 }, ys[2] = { y1, y2 };
	for (int i = 0; i < 2; i++) {
		IOHIDEventRef finger = IOHIDEventCreateDigitizerFingerEvent(kCFAllocatorDefault, ts, i + 1, touch2_identity + i, mask,
			xs[i], ys[i], 0, in ? 1 : 0, 0, in, in, 0);
		IOHIDEventSetIntegerValue(finger, kFieldDigitizerIsDisplayIntegrated, 1);
		IOHIDEventAppendEvent(hand, finger, 0);
		CFRelease(finger);
	}
	dispatch_event(cur_touch, hand);
}

static void send_key(VService* s, uint32_t page, uint32_t usage, bool down) {
	dispatch_event(s, IOHIDEventCreateKeyboardEvent(kCFAllocatorDefault, mach_absolute_time(), page, usage, down, 0));
}

typedef struct { int fd; uint64_t kb_idle_ns; int kb_settle_ms; } Client;  // kb_idle_ns 0: default; kb_settle_ms -1: default

// x86Sim (RAM 2): the time of the last user input, for cli/libexec/jetsam.py (an app the user is touching or typing
// into is never reclaimed): <SIMULATOR_SHARED_RESOURCES_DIRECTORY>/tmp/x86sim-input, its mtime touched at each touch,
// key or button command (at most once a second).
static void note_input(void) {
	static uint64_t last;
	static char path[256];
	uint64_t t = now_ns();
	if (last && t - last < 1000000000ull) return;
	last = t;
	if (!*path) {
		const char* dir = getenv("SIMULATOR_SHARED_RESOURCES_DIRECTORY") ?: getenv("HOME") ?: "/tmp";
		snprintf(path, sizeof(path), "%s/tmp/x86sim-input", dir);
	}
	int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
	if (fd >= 0) { (void)!write(fd, "1\n", 2); close(fd); }
}

// --- Inputs 2: what Apple's simulator offers beyond touches and the basic buttons
// Device orientation, as Xcode's in-device helper dthidd sets it for DeviceHub's Rotate Left / Right and devicectl device
// orientation (BackBoardServices' automation call, UIDeviceOrientation values). We call it from backboardd itself. The
// result also goes to <SIMULATOR_SHARED_RESOURCES_DIRECTORY>/orientation (its name) for the launcher and the CLI.
static const char* const orient_names[7] = { "unknown", "portrait", "portraitUpsideDown", "landscapeLeft", "landscapeRight",
	"faceUp", "faceDown" };
static void (*bks_set_orientation)(int);
static int (*bk_get_orientation)(void), (*bk_get_nonflat_orientation)(void);
static bool load_bks(void) {
	if (bks_set_orientation) return true;
	void* h = dlopen("/System/Library/PrivateFrameworks/BackBoardServices.framework/BackBoardServices", RTLD_NOW);
	bks_set_orientation = h ? dlsym(h, "BKSHIDServicesSetDeviceOrientationForAutomation") : NULL;
	bk_get_orientation = h ? dlsym(h, "BKHIDServicesGetCurrentDeviceOrientation") : NULL;
	bk_get_nonflat_orientation = h ? dlsym(h, "BKHIDServicesGetNonFlatDeviceOrientation") : NULL;
	return bks_set_orientation && bk_get_orientation;
}
static void write_shared_file(const char* name, const char* text) {
	const char* d = getenv("SIMULATOR_SHARED_RESOURCES_DIRECTORY");
	if (!d) return;
	char p[1100], t[1110];
	snprintf(p, sizeof p, "%s/%s", d, name);
	snprintf(t, sizeof t, "%s.tmp", p);
	FILE* f = fopen(t, "w");
	if (!f) return;
	fprintf(f, "%s\n", text);
	fclose(f);
	rename(t, p);
}
// the device orientation (UIDeviceOrientation, 1..6), 1 before BackBoardServices is loaded
static int cur_orientation(void) {
	return bk_get_orientation ? bk_get_orientation() : 1;
}
static void note_orientation(void) {
	int o = bk_get_orientation ? bk_get_orientation() : 0;
	write_shared_file("orientation", orient_names[o >= 0 && o < 7 ? o : 0]);
	slog("orientation now %d (%s)", o, orient_names[o >= 0 && o < 7 ? o : 0]);
}
// orientation <name|1..6> | rotate left|right   (iPhone, iPad: devicectl says "Device Orientation is not supported" for
// Apple's Vision Pro, Apple TV and Apple Watch simulators). Rotate left turns the device counterclockwise: portrait ->
// landscapeLeft -> portraitUpsideDown -> landscapeRight (devicectl device orientation rotate left).
static void set_orientation(const char* arg, int rotate) {
	if (platform != P_IOS) { slog("no orientation on this device"); return; }
	if (!load_bks()) { slog("orientation: BackBoardServices calls not found"); return; }
	int o = -1;
	if (rotate) {
		static const int left_of[7] = { 3, 3, 4, 2, 1, 3, 3 }, right_of[7] = { 4, 4, 3, 1, 2, 4, 4 };
		int cur = bk_get_nonflat_orientation ? bk_get_nonflat_orientation() : bk_get_orientation();
		if (cur < 0 || cur > 6) cur = 1;
		o = rotate < 0 ? left_of[cur] : right_of[cur];
	} else if (arg && *arg) {
		for (int i = 1; i < 7; i++) if (!strcasecmp(arg, orient_names[i])) o = i;
		if (o < 0 && arg[0] >= '1' && arg[0] <= '6' && !arg[1]) o = arg[0] - '0';
		if (o < 0) { slog("unknown orientation %s", arg); return; }
	}
	if (o > 0) { bks_set_orientation(o); slog("orientation -> %d (%s)", o, orient_names[o]); usleep(50000); }
	note_orientation();
	// Models 2: a two-screen phone's relay and device state take the orientation too (a half open landscape pose)
	if (o > 0 && hinge_svc.service) { avp_send_orientation(); post_device_state(hinge_deg); }
}

// Biometrics as SimulatorCoreDevicePlugin does them for DeviceHub's Face ID / Touch ID / Optic ID menus and devicectl device
// simulate biometrics: enrollment is the state of the darwin notification com.apple.BiometricKit.enrollmentChanged
// (1 enrolled), a finger / face / eyes presented is com.apple.BiometricKit_Sim.<fingerTouch|pearl|oyster>.<match|nomatch>.
static void biometric(const char* what, const char* kind) {
	if (!strcmp(what, "enroll") || !strcmp(what, "unenroll")) {
		int tok;
		if (notify_register_check("com.apple.BiometricKit.enrollmentChanged", &tok) == NOTIFY_STATUS_OK) {
			notify_set_state(tok, !strcmp(what, "enroll"));
			notify_cancel(tok);
		}
		notify_post("com.apple.BiometricKit.enrollmentChanged");
		slog("biometric %s", what);
		return;
	}
	bool match = !strcmp(what, "match");
	if (!match && strcmp(what, "nomatch")) { slog("biometric: enroll|unenroll|match|nomatch, not %s", what); return; }
	// the kind the device has: an explicit face|touch|optic, else every kind this platform's BiometricKit knows (it only
	// listens for its own)
	const char* kinds[3] = { NULL, NULL, NULL };
	if (kind && !strcmp(kind, "face")) kinds[0] = "pearl";
	else if (kind && !strcmp(kind, "touch")) kinds[0] = "fingerTouch";
	else if (kind && !strcmp(kind, "optic")) kinds[0] = "oyster";
	else if (platform == P_XR) kinds[0] = "oyster";
	else { kinds[0] = "pearl"; kinds[1] = "fingerTouch"; }
	for (int i = 0; i < 3 && kinds[i]; i++) {
		char n[96];
		snprintf(n, sizeof n, "com.apple.BiometricKit_Sim.%s.%s", kinds[i], match ? "match" : "nomatch");
		notify_post(n);
		slog("biometric: posted %s", n);
	}
}

// The simulated game controller (gamepad_svc): Indigo's order, as DeviceKit fills IndigoHIDMessageForGameControllerEvent
// from a GCExtendedGamepad: up down left right A B X Y L1 R1 L2 R2 LX LY RX RY (buttons 0..1, sticks -1..1, y up)
static double pad[16];
static const char* const pad_names[12] = { "up", "down", "left", "right", "a", "b", "x", "y", "l1", "r1", "l2", "r2" };
static void gamepad_send(void) {
	int32_t f[16];
	for (int i = 0; i < 16; i++) f[i] = (int32_t)(pad[i] * 65536.0);
	dispatch_event(&gamepad_svc, IOHIDEventCreateGameControllerEvent(kCFAllocatorDefault, mach_absolute_time(), 1,
		f[0], f[1], f[2], f[3], f[6], f[7], f[4], f[5], f[12], f[13], f[14], f[15], f[8], f[10], f[9], f[11], 0));
}
// gamepad <button> down|up | gamepad press <button> | gamepad stick left|right <x> <y> | gamepad state <16 values>
static void gamepad_line(const char* line) {
	if (!gamepad_svc.service) { slog("no game controller on this device"); return; }
	char a[16] = "", b[16] = "";
	double x, y;
	if (sscanf(line, "gamepad stick %15s %lf %lf", a, &x, &y) == 3) {
		int i = !strcmp(a, "right") ? 14 : 12;
		pad[i] = x; pad[i + 1] = y;
	} else if (!strncmp(line, "gamepad state", 13)) {
		const char* p = line + 13;
		for (int i = 0; i < 16; i++) {
			int used = 0;
			if (sscanf(p, " %lf%n", &pad[i], &used) != 1) break;
			p += used;
		}
	} else if (sscanf(line, "gamepad %15s %15s", a, b) == 2) {
		bool press = !strcmp(a, "press");
		const char* name = press ? b : a;
		int i = -1;
		for (int k = 0; k < 12; k++) if (!strcasecmp(name, pad_names[k])) i = k;
		if (i < 0) { slog("unknown gamepad button %s", name); return; }
		if (press) {
			pad[i] = 1; gamepad_send();
			pthread_mutex_unlock(&cmd_mu); usleep(100000); pthread_mutex_lock(&cmd_mu);
			pad[i] = 0;
		} else pad[i] = !strcmp(b, "down") ? 1 : 0;
	} else { slog("bad gamepad command: %s", line); return; }
	gamepad_send();
	note_input();
	slog("gamepad %s", line + 8);
}

// The buttons Apple's simulator presses (backboardd's names in brackets, read from its log with Indigo button messages
// sent to Xcode 27's simulators): page << 16 | usage, per platform where they differ
static uint32_t button_usage(const char* n) {
	struct { const char* name; uint32_t ios, tv, watch, xr; } t[] = {
		{ "home", 0xc0040, 0xc0040, 0xc0040, 0xc0040 },        // [Home] Home button; watch: crown press; Vision Pro: crown press
		{ "lock", 0xc0030, 0xc0030, 0xc0030, 0xc0030 },        // [Lock] side / sleep button; tvOS [Power]
		{ "power", 0xc0030, 0xc0030, 0xc0030, 0xc0030 },
		// the Watch's side button is its own usage (CoreDevice ConsumerPage.watchSideButton, Carousel's "pill");
		// an iPhone's side button is Lock
		{ "side", 0xc0030, 0xc0030, 0xc0095, 0xc0030 },
		{ "volup", 0xc00e9, 0xc00e9, 0xc00e9, 0xc00e9 },
		{ "voldown", 0xc00ea, 0xc00ea, 0xc00ea, 0xc00ea },
		{ "siri", 0xc00cf, 0xc0004, 0xc00cf, 0xc00cf },        // [VoiceCommand]; the Siri Remote's is [Microphone]
		{ "action", 0xb002d, 0, 0xff010200, 0xb002d },         // [Action]: iPhone 15 Pro+ (Telephony), Watch Ultra (Apple vendor)
		{ "wallet", 0xc01ba, 0, 0xc01ba, 0xc01ba },            // [DigitalWallet] (Apple Pay double click)
		{ "playpause", 0xc00cd, 0xc00cd, 0xc00cd, 0xc00cd },   // [PlayOrPause]
		{ "tv", 0xc0060, 0xc0060, 0xc0060, 0xc0060 },          // [TV] the Siri Remote's TV / Control Center button
		{ "select", 0xc0080, 0xc0080, 0xc0080, 0xc0080 },      // [Select] the Siri Remote's click
		{ "mute", 0xc00e2, 0xc00e2, 0xc00e2, 0xc00e2 },
		{ "back", 0xc0224, 0xc0224, 0xc0224, 0xc0224 },        // [ACBack]
		// the Siri Remote's Back is GenericDesktop 0x86 [Menu] in Apple's simulator (one level back, as Menu Escape 0x46,
		// which this was until Inputs 2); elsewhere Menu Escape
		{ "menu", 0xc0046, 0x10086, 0xc0046, 0xc0046 },
	};
	for (size_t i = 0; i < sizeof t / sizeof t[0]; i++)
		if (!strcmp(n, t[i].name))
			return platform == P_TV ? t[i].tv : platform == P_WATCH ? t[i].watch : platform == P_XR ? t[i].xr : t[i].ios;
	return 0;
}

// Apple Watch hand gestures, as Apple's watchOS SimulatorHID does with an Indigo watch gesture message: WatchGestures'
// -[WAGSimulatorServerInterface handleGestureType:] (backboardd then delivers a GenericGesture event to the focused
// app / Carousel). Types seen in Apple's watchOS 27.2 simulator: 0 opens the Smart Stack from the watch face (double
// tap), 1 closes it again (wrist flick); 2 and 3 did nothing visible.
static void watch_gesture(int type) {
	static id wag;
	if (!wag) {
		dlopen("/System/Library/PrivateFrameworks/WatchGestures.framework/WatchGestures", RTLD_NOW);
		Class c = objc_getClass("WAGSimulatorServerInterface");
		wag = c ? [[c alloc] init] : nil;
	}
	if (!wag) { slog("gesture: no WAGSimulatorServerInterface"); return; }
	((void (*)(id, SEL, long))objc_msgSend)(wag, sel_registerName("handleGestureType:"), type);
	slog("gesture %d", type);
}

static bool hwkb_on = true;   // the hardware keyboard's connection (hwkb connect|disconnect)
// Keyboard: the hardware keyboard's connection (hwkb, below): GraphicsServices' simulator state, remembered in
// <SIMULATOR_SHARED_RESOURCES_DIRECTORY>/hwkeyboard (connected | disconnected) for the next boot and the host's tools
static void hwkb_set(bool on) {
	int tok;
	hwkb_on = on;
	if (notify_register_check("GSEventHardwareKeyboardAttached", &tok) == NOTIFY_STATUS_OK) {
		notify_set_state(tok, on ? 1 : 0);
		notify_cancel(tok);
	}
	notify_post("GSEventHardwareKeyboardAttached");
	write_shared_file("hwkeyboard", on ? "connected" : "disconnected");
	slog("hardware keyboard %s", on ? "connected" : "disconnected");
}
// backboardd attaches the hardware keyboard itself whenever a keyboard service arrives (GSEventHardwareKeyboardAttached
// went 0 -> 1 at our on-demand keyboard's first key): while disconnected it is put back at once, so keys of a
// disconnected keyboard still type (as in Apple's simulator) and the software keyboard stays up
static void hwkb_watch(void) {
	int tok;
	notify_register_dispatch("GSEventHardwareKeyboardAttached", &tok, dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE, 0), ^(int t) {
		uint64_t st = 0;
		notify_get_state(t, &st);
		pthread_mutex_lock(&cmd_mu);
		if (!hwkb_on && (st & 1)) {
			slog("hardware keyboard attached by the system (state %llu) while disconnected: detached again", (unsigned long long)st);
			hwkb_set(false);
		}
		pthread_mutex_unlock(&cmd_mu);
	});
}
static bool hwkb_saved_on(void) {
	const char* d = getenv("SIMULATOR_SHARED_RESOURCES_DIRECTORY");
	char p[1100], t[32] = "";
	if (!d) return true;
	snprintf(p, sizeof p, "%s/hwkeyboard", d);
	FILE* f = fopen(p, "r");
	if (!f) return true;
	if (!fgets(t, sizeof t, f)) *t = 0;
	fclose(f);
	return strncmp(t, "disconnected", 12) != 0;
}

static bool handle_inputs2(char* line) {
	char a[32] = "", b[32] = "";
	if (!strcmp(line, "appswitcher")) {
		// DeviceHub's App Switcher: the home button pressed twice in quick succession (SpringBoard counts the presses
		// itself for a sender without press counting: "SpringBoard is providing emulation"), also on Face ID models
		uint32_t u = button_usage("home");
		for (int i = 0; i < 2; i++) {
			send_key(&buttons_svc, u >> 16, u & 0xffff, true);
			pthread_mutex_unlock(&cmd_mu); usleep(60000); pthread_mutex_lock(&cmd_mu);
			send_key(&buttons_svc, u >> 16, u & 0xffff, false);
			if (!i) { pthread_mutex_unlock(&cmd_mu); usleep(80000); pthread_mutex_lock(&cmd_mu); }
		}
		note_input();
		slog("appswitcher");
		return true;
	}
	if (sscanf(line, "standby %31s", a) == 1) {
		// iPhone StandBy, as SimulatorCoreDevicePlugin's StandBy show / hide: the state of the darwin notification
		// com.apple.springboard.ambient.presentation (1 showing) + a post; SpringBoard presents it when the phone is
		// locked and in landscape (Apple's simulator too)
		if (platform != P_IOS) { slog("no StandBy on this device"); return true; }
		int tok;
		if (notify_register_check("com.apple.springboard.ambient.presentation", &tok) == NOTIFY_STATUS_OK) {
			notify_set_state(tok, !strcmp(a, "show"));
			notify_cancel(tok);
		}
		notify_post("com.apple.springboard.ambient.presentation");
		slog("standby %s", a);
		return true;
	}
	if (sscanf(line, "gesture %31s", a) == 1) {
		if (platform != P_WATCH) { slog("no hand gestures on this device"); return true; }
		watch_gesture(!strcmp(a, "doubletap") ? 0 : !strcmp(a, "wristflick") ? 1 : atoi(a));
		note_input();
		return true;
	}
	if (sscanf(line, "hwkb %31s", a) == 1) {
		// Keyboard: I/O > Keyboard > Connect Hardware Keyboard, as CoreSimulator's -[SimDevice
		// setHardwareKeyboardEnabled:keyboardType:error:] does it: CoreSimulatorBridge calls GraphicsServices'
		// GSEventSetHardwareKeyboardAttachedWithCountryCodeAndType(enabled, 0, type), which in a simulator is the state
		// of the darwin notification GSEventHardwareKeyboardAttached (attached | country << 8 | type << 16) + a post
		// (read in Apple's iOS 27.2 simulator: 1 at boot, 0 after a disconnect). UIKit reads it: disconnected, the
		// software keyboard shows on every focus and stays up while keys come in (they still type).
		if (!strcmp(a, "connect") || !strcmp(a, "on")) hwkb_set(true);
		else if (!strcmp(a, "disconnect") || !strcmp(a, "off")) hwkb_set(false);
		else slog("hwkb: connect|disconnect, not %s", a);
		return true;
	}
	if (!strcmp(line, "shake")) {
		// as DeviceHub's Shake (SimFeatureUIKitShakeDarwinNotification): UIKit turns it into a motion event for the key window
		notify_post("com.apple.UIKit.SimulatorShake");
		note_input();
		slog("shake");
		return true;
	}
	if (sscanf(line, "rotate %31s", a) == 1) {
		set_orientation(NULL, !strcmp(a, "left") ? -1 : 1);
		note_input();
		return true;
	}
	if (!strncmp(line, "orientation", 11) && (line[11] == 0 || line[11] == ' ')) {
		sscanf(line + 11, " %31s", a);
		set_orientation(a, 0);
		return true;
	}
	int hs;
	if (sscanf(line, "hingestream %d", &hs) == 1) { hinge_stream_ms = hs < 0 ? 0 : hs; slog("hinge stream every %d ms", hinge_stream_ms); return true; }
	if (sscanf(line, "biometric %31s %31s", a, b) >= 1) {
		biometric(a, *b ? b : NULL);
		return true;
	}
	if (!strncmp(line, "gamepad ", 8)) { gamepad_line(line); return true; }
	return false;
}

static void handle_line(char* line, Client* cl) {
	unsigned scr;
	if (handle_inputs2(line)) return;
	int skip = 0;
	if (line[0] == '@' && sscanf(line, "@%u %n", &scr, &skip) == 1 && skip > 0) {   // Models 2: another display's touch
		if (scr != 1 && (!inner_touch_svc.service || scr != inner_display_id)) { slog("no touchscreen for display %u", scr); return; }
		cur_touch = scr == 1 ? &touch_svc : &inner_touch_svc;
		handle_line(line + skip, cl);
		cur_touch = &touch_svc;
		return;
	}
	double deg;
	int ms = 600;
	if (!strcmp(line, "fold") || !strcmp(line, "unfold") || sscanf(line, "hinge %lf %d", &deg, &ms) >= 1) {
		if (!hinge_svc.service) { slog("no hinge on this device: %s", line); return; }
		double to = !strcmp(line, "fold") ? 0 : !strcmp(line, "unfold") ? 180 : deg < 0 ? 0 : deg > 180 ? 180 : deg;
		// a hinge moving as a hand opens it: 100 Hz reports (the ReportInterval CoreMotion sets) over <ms> with the
		// velocity, phase Began / Changed / Ended (IOHIDEventPhase 1 / 2 / 4); the device state follows the angle.
		// At least a report per degree, so every whole degree on the way is seen (the relay takes whole degrees):
		// a fast move reports faster than 100 Hz rather than skipping angles.
		double from = hinge_deg;
		if (ms < 0) ms = 0;
		int steps = ms > 0 ? (ms + 9) / 10 : 0;
		int per_deg = (int)ceil(fabs(to - from));
		if (ms > 0 && per_deg > steps) steps = per_deg;
		useconds_t step_us = steps > 0 ? (useconds_t)(ms * 1000 / steps) : 0;
		slog("hinge %.1f -> %.1f in %d ms", from, to, ms);
		avp_send_pose();   // the relay's other inputs (it may have started after the last command)
		hinge_moving = 1;
		for (int i = 1; i <= steps; i++) {
			hinge_deg = from + (to - from) * i / steps;
			hinge_vel = ms > 0 ? (to - from) * 1000.0 / ms : 0;
			hinge_phase = i == 1 ? 1 : 2;
			dispatch_event(&hinge_svc, hinge_event());
			avp_send_angle(hinge_deg);   // as Simulator's hinge slider while it is dragged
			if (i == 1) post_device_state(hinge_deg);
			pthread_mutex_unlock(&cmd_mu);
			usleep(step_us);
			pthread_mutex_lock(&cmd_mu);
		}
		hinge_moving = 0;
		hinge_deg = to; hinge_vel = 0; hinge_phase = 4;
		dispatch_event(&hinge_svc, hinge_event());
		avp_send_angle(to);
		hinge_phase = 2;
		post_device_state(hinge_deg);
		return;
	}
	char a[16] = "", b[16] = "";
	double x = 0, y = 0;
	double x2 = 0, y2 = 0;
	if (!strncmp(line, "touch", 5) || !strncmp(line, "key ", 4) || !strncmp(line, "button ", 7)) note_input();
	if (sscanf(line, "touch2 %15s %lf %lf %lf %lf", a, &x, &y, &x2, &y2) == 5) { send_touch2(a, x, y, x2, y2); return; }
	if (!paloma_svc.service && sscanf(line, "touch %15s %lf %lf %15s", a, &x, &y, b) >= 3) {   // visionOS: look and pinch, below
		static const char* edges[5] = { "none", "top", "left", "bottom", "right" };
		int edge = 0;
		for (int i = 1; i < 5; i++) if (!strcmp(b, edges[i])) edge = i;
		send_touch(a, x, y, edge);
		return;
	}
	unsigned usage, kpage = 7;
	if (sscanf(line, "key %15s %i %i", a, &usage, &kpage) >= 2) {
		// x86Sim arm64: the keyboard service is attached at the first key command (see initUsingHIDService).
		// Speed 31 held this client's keys for a fixed 1.5 s after every attach (keys 300 ms after the first were
		// lost or scrambled). Input: host keyboard latency (kblat.py) found the attach itself is ready in ~0.1 s
		// (backboardd sets the service's properties within 110 ms) and a burst typed 0 ms after it lands whole; what
		// scrambles keys is a field still taking focus: keys typed < ~0.7 s after the tap that focused it land
		// reordered ("settingsifi") whether or not the keyboard was attached just then. So: a short attach settle
		// (SIMHID_KB_SETTLE_MS, default 150) and a focus guard: a key within SIMHID_KB_TOUCH_GUARD_MS (default 1000)
		// of the last touch lift waits for the rest of it. Every other key goes out at once (was: the first burst
		// after each attach, i.e. after every 5 min pause in the launcher, came 1.2-1.5 s late).
		// The wait does not hold cmd_mu (the caller's lock is dropped around it): touches and keys of other clients
		// and the idle thread go on. This client's later lines queue in the socket, in order.
		int64_t wait_ns = 0;
		if (!keyboard_svc.service && hid_system) {
			add_service(hid_system, &keyboard_svc);
			const char* ms = getenv("SIMHID_KB_SETTLE_MS");
			wait_ns = (int64_t)(cl && cl->kb_settle_ms >= 0 ? cl->kb_settle_ms : ms && *ms ? atoi(ms) : 150) * 1000000;
			// r2: the first attach of a boot needs longer. UIKit's switch into hardware-keyboard mode is cold then (its
			// code not yet translated in the processes involved): Shift + h e l l o typed 150 ms after it came out as
			// "HELLO WORLD" (from the browser view and from a plain simhid client alike), 500 ms and 1000 ms
			// after it as typed; every later attach in the boot, also the first one in another app, typed right at
			// 150 ms. SIMHID_KB_FIRST_SETTLE_MS (default 800) for that first one, a client's kbsettle still wins.
			static int attaches;
			if (!attaches++ && !(cl && cl->kb_settle_ms >= 0)) {
				const char* f = getenv("SIMHID_KB_FIRST_SETTLE_MS");
				const int64_t first = (int64_t)(f && *f ? atoi(f) : 800) * 1000000;
				if (first > wait_ns) wait_ns = first;
			}
		}
		{
			static int64_t guard = -1;
			if (guard < 0) { const char* g = getenv("SIMHID_KB_TOUCH_GUARD_MS"); guard = (int64_t)(g && *g ? atoi(g) : 1000) * 1000000; }
			const int64_t since = touch_up_ns ? (int64_t)(now_ns() - touch_up_ns) : INT64_MAX;
			if (guard - since > wait_ns) wait_ns = guard - since;
		}
		if (wait_ns > 0) {
			// kb_last_ns / kb_down first so the idle thread cannot detach the service meanwhile
			kb_last_ns = now_ns(); kb_down++;
			pthread_mutex_unlock(&cmd_mu);
			usleep((useconds_t)(wait_ns / 1000));
			pthread_mutex_lock(&cmd_mu);
			kb_down--;
			slog("key held %lld ms (attach settle / focus guard)", (long long)(wait_ns / 1000000));
		}
		kb_last_ns = now_ns();
		kb_idle_ns = cl ? cl->kb_idle_ns : 0;
		kb_down += !strcmp(a, "down") ? 1 : -1;
		if (kb_down < 0) kb_down = 0;
		pthread_cond_signal(&kb_cv);
		// a page after the usage: another page's key on the keyboard (12 0x1ae: the on-screen keyboard key, softkb)
		send_key(&keyboard_svc, kpage, usage, !strcmp(a, "down"));
		return;
	}
	int secs;
	if (sscanf(line, "kbsettle %d", &secs) == 1) {
		if (cl) cl->kb_settle_ms = secs;
		slog("client fd=%d kbsettle %d", cl ? cl->fd : -1, secs);
		return;
	}
	if (sscanf(line, "kbidle %d", &secs) == 1) {
		// <= 0: never detach after this client's keys (like SIMHID_KB_IDLE_S=0)
		if (cl) cl->kb_idle_ns = secs > 0 ? (uint64_t)secs * 1000000000ull : ~0ull;
		slog("client fd=%d kbidle %d", cl ? cl->fd : -1, secs);
		return;
	}
	if (paloma_svc.service && !strncmp(line, "touch ", 6)) {
		// Platforms 2: on visionOS a touch is look and pinch (the launcher, MCP and scripts send touches): down looks at
		// the point and pinches, moves drag the pinching hand by the touch's travel (scrolling and dragging), up lets go
		double x, y; char ph[16];
		if (sscanf(line, "touch %15s %lf %lf", ph, &x, &y) == 3) {
			static float d0[3];
			static double x0, y0;
			static const float o0[3] = { 0, 0, 0 };
			if (!strcmp(ph, "down")) {
				display_dir(x, y, d0); x0 = x; y0 = y;
				for (int i = 0; i < 3; i++) { paloma_send(o0, d0, 1, 0, 0, 0, 0); usleep(16000); }
				paloma_send_hand(o0, d0, 1, 0, 0);
			} else if (!strcmp(ph, "move")) {
				// about half a metre of hand travel across the whole display
				paloma_send_hand(o0, d0, 1, (float)((x - x0) * 0.5), (float)((y0 - y) * 0.3));
			} else {
				paloma_send_hand(o0, d0, 0, (float)((x - x0) * 0.5), (float)((y0 - y) * 0.3));
				usleep(16000);
				paloma_send(o0, d0, 1, 0, 0, 0, 0);
			}
			note_input();
			return;
		}
	}
	{   // tap x y | hover x y | pinch down|up x y (Platforms 2: visionOS look and pinch at a display point)
		double x, y; char ph[8] = "";
		int t = sscanf(line, "tap %lf %lf", &x, &y) == 2 ? 1 : sscanf(line, "hover %lf %lf", &x, &y) == 2 ? 2
			: sscanf(line, "pinch %7s %lf %lf", ph, &x, &y) == 3 ? 3 : 0;
		if (t) {
			if (!paloma_svc.service) { slog("no natural input on this device: %s", line); return; }
			static const float o0[3] = { 0, 0, 0 };
			float d[3];
			display_dir(x, y, d);
			if (t == 1) {
				for (int i = 0; i < 4; i++) { paloma_send(o0, d, 1, 0, 0, 0, 0); usleep(16000); }
				for (int i = 0; i < 5; i++) { paloma_send(o0, d, 1, 0, 1, 0, 0); usleep(16000); }
				for (int i = 0; i < 4; i++) { paloma_send(o0, d, 1, 0, 0, 0, 0); usleep(16000); }
			} else if (t == 2) {
				paloma_send(o0, d, 1, 0, 0, 0, 0);
			} else {
				paloma_send(o0, d, 1, 0, !strcmp(ph, "down") || !strcmp(ph, "move"), 0, 0);
			}
			if (t != 2) slog("%s (%.3f,%.3f) -> dir (%.3f %.3f) proj %d", t == 1 ? "tap" : "pinch", x, y, d[0], d[1], have_proj);
			note_input();
			return;
		}
	}
	if (!strncmp(line, "gaze ", 5)) {   // gaze ox oy oz dx dy dz [hands pinchL pinchR phaseL phaseR]
		if (!paloma_svc.service) { slog("no natural input on this device: %s", line); return; }
		float o3[3], d3[3]; int hands = 0, pl = 0, pr = 0, phl = 0, phr = 0;
		int nf = sscanf(line, "gaze %f %f %f %f %f %f %d %d %d %d %d", &o3[0], &o3[1], &o3[2], &d3[0], &d3[1], &d3[2], &hands, &pl, &pr, &phl, &phr);
		if (nf < 6) { slog("bad gaze command: %s", line); return; }
		paloma_send(o3, d3, hands, pl, pr, (uint16_t)phl, (uint16_t)phr);
		slog("gaze (%.3f %.3f %.3f) -> (%.3f %.3f %.3f) hands %d pinch %d/%d", o3[0], o3[1], o3[2], d3[0], d3[1], d3[2], hands, pl, pr);
		note_input();
		return;
	}
	if (!strcmp(line, "trackpad on") || !strcmp(line, "trackpad off")) {
		// Inputs 2: an iPad's trackpad, as Apple's "Simulate Trackpad" adds it (IndigoHIDMessageToCreatePointerService,
		// target 0x35; read with a probe after that message): Digitizer Touch Pad with the Apple vendor and mouse usages,
		// not built in. iPadOS then shows Settings > General > Trackpad. Not driven: Apple's simulator gets trackpad
		// gestures from the Mac's own trackpad events, and none of the ones built here (scroll / scale / rotation /
		// dock-swipe events, alone or as children of finger frames; the same through Apple's Indigo trackpad message)
		// moved anything in Apple's iPad simulator or here. Pinch / two-finger pan / edge swipes are touch gestures.
		const char* model = getenv("SIMULATOR_MODEL_IDENTIFIER");
		const bool on = !strcmp(line, "trackpad on");
		if (on && !trackpad_svc.service && platform == P_IOS && model && !strncmp(model, "iPad", 4) && hid_system) {
			trackpad_svc = (VService){ "trackpad", 0x7863000000000010ull, (CFDictionaryRef)[@{
				@"DeviceUsagePairs": @[ @{ @"DeviceUsagePage": @13, @"DeviceUsage": @12 }, @{ @"DeviceUsagePage": @0xff00, @"DeviceUsage": @1 },
					@{ @"DeviceUsagePage": @0xff00, @"DeviceUsage": @12 }, @{ @"DeviceUsagePage": @1, @"DeviceUsage": @1 },
					@{ @"DeviceUsagePage": @1, @"DeviceUsage": @2 }, @{ @"DeviceUsagePage": @13, @"DeviceUsage": @5 } ],
				@"PrimaryUsagePage": @13, @"PrimaryUsage": @12, @"Built-In": @NO } retain] };
			add_service(hid_system, &trackpad_svc);
		} else if (!on && trackpad_svc.service && hid_system) {
			_IOHIDEventSystemRemoveService(hid_system, trackpad_svc.service);
			_IOHIDServiceTerminate(trackpad_svc.service);
			CFRelease(trackpad_svc.service);
			trackpad_svc.service = NULL;
			slog("trackpad removed");
		} else if (on && !trackpad_svc.service) slog("no trackpad on this device");
		return;
	}
	if (!strncmp(line, "pointer ", 8)) {
		// Inputs 2: an iPad's mouse, as Apple's simulator adds it when the host captures its pointer
		// (IndigoHIDMessageToCreateMouseService, target 0x36: GenericDesktop Mouse with the Pointer usage too, not built in;
		// read with a probe after that message). Added at the first pointer command;
		// iPadOS then draws its pointer. "pointer remove" takes it away again (IndigoHIDMessageToRemovePointerService).
		const char* model = getenv("SIMULATOR_MODEL_IDENTIFIER");
		if (!strcmp(line, "pointer remove")) {
			if (platform == P_IOS && pointer_svc.service && hid_system) {
				_IOHIDEventSystemRemoveService(hid_system, pointer_svc.service);
				_IOHIDServiceTerminate(pointer_svc.service);
				CFRelease(pointer_svc.service);
				pointer_svc.service = NULL;
				slog("mouse removed");
			}
			return;
		}
		if (!pointer_svc.service && platform == P_IOS && model && !strncmp(model, "iPad", 4) && hid_system) {
			pointer_svc = (VService){ "mouse", 0x786300000000000full, (CFDictionaryRef)[@{
				@"DeviceUsagePairs": @[ @{ @"DeviceUsagePage": @1, @"DeviceUsage": @1 }, @{ @"DeviceUsagePage": @1, @"DeviceUsage": @2 } ],
				@"PrimaryUsagePage": @1, @"PrimaryUsage": @2, @"Built-In": @NO } retain] };
			add_service(hid_system, &pointer_svc);
		}
		if (!pointer_svc.service) { slog("no pointer on this device: %s", line); return; }
		double dx = 0, dy = 0;
		uint32_t old = pointer_buttons;
		if (sscanf(line, "pointer at %lf %lf", &dx, &dy) == 2) {
			// Inputs 2: the pointer put at a point of the display (normalized 0..1, the panel's own orientation): the pointer
			// event with kIOHIDEventOptionIsAbsolute (bit 0), which backboardd takes as a position, without the speed-up it
			// gives relative moves (one +300 moved the iPad's pointer 450 points, 30 moves of 10 305). The launcher's pointer
			// capture uses it so the pointer stays under the host's cursor.
			dispatch_event(&pointer_svc, IOHIDEventCreateRelativePointerEvent(kCFAllocatorDefault, mach_absolute_time(), dx, dy, 0,
				pointer_buttons, pointer_buttons, 1));
			note_input();
			return;
		}
		if (sscanf(line, "pointer scroll %lf %lf", &dx, &dy) == 2) {
			// the wheel / two-finger scroll: Apple's SimulatorHID makes a scroll event with the host event's phase (and
			// momentum); a UIKit scroll view follows only a phased one: Began, the deltas in a few Changed, Ended
			static const uint16_t ph[5] = { 1, 2, 2, 2, 4 };
			for (int i = 0; i < 5; i++) {
				const double f = ph[i] == 2 ? 1.0 / 3 : 0;
				IOHIDEventRef e = IOHIDEventCreateScrollEvent(kCFAllocatorDefault, mach_absolute_time(), dx * f, dy * f, 0, 0);
				IOHIDEventSetPhase(e, ph[i]);
				dispatch_event(&pointer_svc, e);
				pthread_mutex_unlock(&cmd_mu); usleep(16000); pthread_mutex_lock(&cmd_mu);
			}
			note_input();
			return;
		}
		if (sscanf(line, "pointer move %lf %lf", &dx, &dy) == 2) {
		} else if (sscanf(line, "pointer button %15s", a) == 1) {
			pointer_buttons = !strcmp(a, "down") ? 1 : 0;
		} else if (!strcmp(line, "pointer click")) {
			dispatch_event(&pointer_svc, IOHIDEventCreateRelativePointerEvent(kCFAllocatorDefault, mach_absolute_time(), 0, 0, 0, 1, 0, 0));
			usleep(60000);
			dispatch_event(&pointer_svc, IOHIDEventCreateRelativePointerEvent(kCFAllocatorDefault, mach_absolute_time(), 0, 0, 0, 0, 1, 0));
			pointer_buttons = 0;
			slog("pointer click");
			note_input();
			return;
		} else { slog("bad pointer command: %s", line); return; }
		dispatch_event(&pointer_svc, IOHIDEventCreateRelativePointerEvent(kCFAllocatorDefault, mach_absolute_time(), dx, dy, 0,
			pointer_buttons, old, 0));
		note_input();
		return;
	}
	double delta;
	int nf = sscanf(line, "crown %lf %15s", &delta, a);
	if (nf >= 1) {
		// Inputs 2: a Vision Pro's crown is the Dial service (IndigoHIDMessageForDigitalDialEvent: the same scroll event)
		VService* cs = crown_svc.service ? &crown_svc : &dial_svc;
		if (!cs->service) { slog("no digital crown on this device: %s", line); return; }
		note_input();
		const char axis = nf == 2 ? a[0] : 'y';
		dispatch_event(cs, IOHIDEventCreateScrollEvent(kCFAllocatorDefault, mach_absolute_time(),
			axis == 'x' ? delta : 0, axis == 'y' ? delta : 0, axis == 'z' ? delta : 0, 0));
		slog("crown %.2f (%c)", delta, axis);
		return;
	}
	unsigned page;
	if (sscanf(line, "usage %i %i %15s", &page, &usage, a) == 3) {   // Platforms 1: any page/usage on the buttons service
		note_input();
		send_key(&buttons_svc, page, usage, !strcmp(a, "down"));
		slog("usage 0x%x/0x%x %s", page, usage, a);
		return;
	}
	if (sscanf(line, "button %15s %15s", a, b) == 2) {
		bool down = !strcmp(b, "down");
		// consumer page: 0x40 menu (home), 0x30 power (lock), 0xe9/0xea volume.
		// Platforms 1: on tvOS "menu" is the Siri Remote's Back: 0x46 (Menu Escape) goes back one level, as the
		// keyboard's Escape does; 0x40 (Menu) closed the app from any depth. 0x60 is its TV button (PineBoard's home;
		// found by trying usages, see SIMHID.md), 0xcd play / pause. On a watch 0x40 is the crown's press.
		// Inputs 2: the table is button_usage (Apple's usages per platform)
		uint32_t u = button_usage(a);
		if (u) { send_key(&buttons_svc, u >> 16, u & 0xffff, down); slog("button %s %s (0x%x/0x%x)", a, b, u >> 16, u & 0xffff); }
		else slog("unknown button on this device: %s", a);
		return;
	}
	if (*line) slog("unknown command: %s", line);
}

static void* client_thread(void* arg) {
	int fd = (int)(intptr_t)arg;
	Client cl = { fd, 0, -1 };
	slog("client_thread fd=%d", fd);
	char buf[4096];
	size_t n = 0;
	for (;;) {
		ssize_t r = read(fd, buf + n, sizeof(buf) - 1 - n);
		if (r <= 0) break;
		n += r;
		buf[n] = 0;
		char* start = buf;
		char* nl;
		while ((nl = strchr(start, '\n'))) {
			*nl = 0;
			if (nl > start && nl[-1] == '\r') nl[-1] = 0;
			pthread_mutex_lock(&cmd_mu);
			handle_line(start, &cl);
			pthread_mutex_unlock(&cmd_mu);
			start = nl + 1;
		}
		n = strlen(start);
		memmove(buf, start, n);
		if (n >= sizeof(buf) - 1) n = 0;
	}
	slog("client_thread fd=%d done", fd);
	close(fd);
	return NULL;
}

// A socket path that does not fit sockaddr_un.sun_path (104 bytes on Darwin; the Linux path plus Darling's
// /Volumes/SystemRoot, e.g. a long home or device name) used to be cut short by strlcpy: unlink removed the real path,
// bind made a truncated sibling, and the next boot failed with EADDRINUSE. Such a socket now lives next to the phone's
// frame ring (SIMFB_RING's directory, /run/user/<uid>), named after the phone, and the expected path becomes a symlink
// to its Linux path: the clients (simview, simtouch, the launcher) are on Linux, nothing in the guest connects.
// Reported with a fix by Prathamesh913 (x86sim/x86sim#1).
static const char kSystemRoot[] = "/Volumes/SystemRoot";

static bool short_socket(const char* path, char* out, size_t n) {
	char dir[256];
	const char* ring = getenv("SIMFB_RING");
	const char* slash = ring ? strrchr(ring, '/') : NULL;
	if (slash && strncmp(ring, kSystemRoot, sizeof(kSystemRoot) - 1) == 0)
		snprintf(dir, sizeof(dir), "%.*s", (int)(slash - ring), ring);
	else
		snprintf(dir, sizeof(dir), "%s/run/user/%u", kSystemRoot, (unsigned)getuid());
	const char* udid = getenv("SIMULATOR_UDID");
	uint64_t h = 1469598103934665603ull;  // FNV-1a of the path, for a phone without a UDID
	for (const char* p = path; *p; p++) h = (h ^ (uint8_t)*p) * 1099511628211ull;
	int w = udid && *udid ? snprintf(out, n, "%s/x86sim-simhid-%s.sock", dir, udid)
	                      : snprintf(out, n, "%s/x86sim-simhid-%016llx.sock", dir, (unsigned long long)h);
	return w > 0 && (size_t)w < n;
}

static void* listen_thread(void* arg) {
	char path[256], bound[256];
	struct sockaddr_un a = { 0 };
	const char* env = getenv("SIMHID_SOCKET");
	const char* dir = getenv("SIMULATOR_SHARED_RESOURCES_DIRECTORY") ?: getenv("HOME") ?: "/tmp";
	if (env) snprintf(path, sizeof(path), "%s", env); else snprintf(path, sizeof(path), "%s/simhid.sock", dir);
	bool linked = strlen(path) >= sizeof(a.sun_path);
	if (linked && !short_socket(path, bound, sizeof(a.sun_path))) {
		slog("cannot listen on %s: path too long (%zu bytes) and no short path fits", path, strlen(path));
		return NULL;
	}
	if (!linked) snprintf(bound, sizeof(bound), "%s", path);
	int s = socket(AF_UNIX, SOCK_STREAM, 0);
	a.sun_family = AF_UNIX;
	strlcpy(a.sun_path, bound, sizeof(a.sun_path));
	unlink(bound);
	if (bind(s, (struct sockaddr*)&a, sizeof(a)) != 0 || listen(s, 8) != 0) {
		slog("cannot listen on %s: errno %d", bound, errno);
		return NULL;
	}
	if (linked) {
		// the link names the Linux path; the guest's view of it (/Volumes/SystemRoot/...) means nothing to the clients
		const char* target = strncmp(bound, kSystemRoot, sizeof(kSystemRoot) - 1) == 0 ? bound + sizeof(kSystemRoot) - 1 : bound;
		unlink(path);
		if (symlink(target, path) != 0) slog("cannot link %s -> %s: errno %d", path, target, errno);
		slog("listening on %s (%s is too long for sun_path)", target, path);
	} else {
		slog("listening on %s", path);
	}
	for (;;) {
		// x86Sim: Darling's accept() crashes with a NULL address (it converts the sockaddr back unconditionally)
		struct sockaddr_un peer; socklen_t plen = sizeof(peer);
		int c = accept(s, (struct sockaddr*)&peer, &plen);
		if (c < 0) { if (errno == EINTR) continue; slog("accept: errno %d", errno); usleep(100000); continue; }
		pthread_t t;
		pthread_create(&t, NULL, client_thread, (void*)(intptr_t)c);
		pthread_detach(t);
	}
	return NULL;
}

@protocol SimulatorClientHIDSystemManager <NSObject>
- (instancetype)initUsingHIDService:(IOHIDEventSystemRef_)system error:(NSError**)error;
@end

@interface SimHIDSystem : NSObject <SimulatorClientHIDSystemManager>
@end

@implementation SimHIDSystem
- (instancetype)initUsingHIDService:(IOHIDEventSystemRef_)system error:(NSError**)error {
	if (!(self = [super init])) return nil;
	if (!system) { if (error) *error = [NSError errorWithDomain:@"SimHID" code:1 userInfo:nil]; [self release]; return nil; }
	// displayUUID must match SFBDisplayGetDisplayUID ("x86Sim-main-display"): BKIOHIDService
	// _setUpInitialProperties copies the HID property "displayUUID" into its ivar; without it,
	// BKDisplayConvertCAScreenLocationToContext gets a null UUID and drops every touch.
	touch_svc = (VService){ "touchscreen", 0x7863000000000001ull,
		make_props(@"com.apple.SimulatorHID.ScreenTouchService.x86Sim-main-display", 0x0d, 0x04, @{
			@"DisplayIntegrated": @YES,
			@"SupportsInk": @NO,
			@"displayUUID": @"x86Sim-main-display",
		}) };
	const char* model = getenv("SIMULATOR_MODEL_IDENTIFIER");
	platform = !model ? P_IOS : !strncmp(model, "AppleTV", 7) ? P_TV : !strncmp(model, "Watch", 5) ? P_WATCH
		: !strncmp(model, "RealityDevice", 13) ? P_XR : P_IOS;
	// Inputs 2: the buttons service with the properties of Apple's per platform: "MainScreenButtonsService" (Telephony
	// page, built in, authenticated; backboardd counts home presses for iPhone / Vision Pro), on an Apple TV the Siri
	// Remote's buttons (no main-screen buttons there). Was: consumer page 1, Transport Virtual, vendor 0x5ac, everywhere.
	NSDictionary* tel = @{ @"DeviceUsagePairs": @[ @{ @"DeviceUsagePage": @11, @"DeviceUsage": @1 } ],
		@"PrimaryUsagePage": @11, @"PrimaryUsage": @1, @"Built-In": @YES, @"Authenticated": @YES };
	NSDictionary* remote = @{ @"VendorID": @76, @"ProductID": @614, @"PhysicalDeviceUniqueID": @"DF45F209-BE9E-43D0-8B43-792D8E0A698B" };
	NSMutableDictionary* bp = [NSMutableDictionary dictionaryWithDictionary:platform == P_TV ? remote : tel];
	if (platform == P_TV)
		[bp addEntriesFromDictionary:@{ @"DeviceUsagePairs": @[ @{ @"DeviceUsagePage": @12, @"DeviceUsage": @1 } ],
			@"PrimaryUsagePage": @12, @"PrimaryUsage": @1 }];
	else if (platform != P_WATCH) {
		const bool xr = platform == P_XR;
		[bp addEntriesFromDictionary:@{ @"PressCountTrackingEnabled": @(xr), @"PressCountUsagePairs": @[ @0xc0040 ],
			@"PressCountDoublePressTimeout": @(xr ? 2000000 : 0), @"PressCountTriplePressTimeout": @(xr ? 2000000 : 0) }];
	}
	buttons_svc = (VService){ "buttons", 0x7863000000000002ull, (CFDictionaryRef)[bp retain] };
	keyboard_svc = (VService){ "keyboard", 0x7863000000000003ull, make_props(@"x86Sim keyboard", 0x01, 0x06, nil) };
	add_service(system, &touch_svc);
	add_service(system, &buttons_svc);
	{
		if (platform == P_TV) {   // the Siri Remote's touch surface and motion, beside its buttons
			NSMutableDictionary* d = [NSMutableDictionary dictionaryWithDictionary:remote];
			[d addEntriesFromDictionary:@{ @"DeviceUsagePairs": @[ @{ @"DeviceUsagePage": @13, @"DeviceUsage": @5 },
				@{ @"DeviceUsagePage": @13, @"DeviceUsage": @12 } ], @"PrimaryUsagePage": @13, @"PrimaryUsage": @5 }];
			remote_digitizer_svc = (VService){ "remote-digitizer", 0x786300000000000bull, (CFDictionaryRef)[d retain] };
			d = [NSMutableDictionary dictionaryWithDictionary:remote];
			[d addEntriesFromDictionary:@{ @"DeviceUsagePairs": @[ @{ @"DeviceUsagePage": @0xff00, @"DeviceUsage": @0x10 } ],
				@"PrimaryUsagePage": @0xff00, @"PrimaryUsage": @0x10 }];
			remote_motion_svc = (VService){ "remote-motion", 0x786300000000000cull, (CFDictionaryRef)[d retain] };
			add_service(system, &remote_digitizer_svc);
			add_service(system, &remote_motion_svc);
		}
		if (platform == P_IOS || platform == P_XR) {
			proximity_svc = (VService){ "proximity", 0x786300000000000dull, (CFDictionaryRef)[@{
				@"DeviceUsagePairs": @[ @{ @"DeviceUsagePage": @0xff00, @"DeviceUsage": @8 } ],
				@"PrimaryUsagePage": @0xff00, @"PrimaryUsage": @8, @"Built-In": @YES } retain] };
			add_service(system, &proximity_svc);
		}
		if (platform == P_XR) {
			dial_svc = (VService){ "dial", 0x786300000000000eull, (CFDictionaryRef)[@{
				@"DeviceUsagePairs": @[ @{ @"DeviceUsagePage": @1, @"DeviceUsage": @0x37 } ],
				@"PrimaryUsagePage": @1, @"PrimaryUsage": @0x37, @"Built-In": @YES } retain] };
			add_service(system, &dial_svc);
		}
		if (platform == P_XR) {
			pointer_svc = (VService){ "pointer", 0x7863000000000007ull,
				make_props(@"x86Sim pointer", 0x01, 0x02, @{ @"Transport": @"Virtual-AssistiveTouch", @"Built-In": @NO }) };
			add_service(system, &pointer_svc);
			paloma_svc = (VService){ "paloma", 0x7863000000000008ull, make_props(@"x86Sim natural input", 0xff00, 0x6f, nil) };
			add_service(system, &paloma_svc);
			_dyld_register_func_for_add_image(hmd_hook);
		}
		// Inputs 2: Apple's iOS and visionOS SimulatorHID add the same game controller (not watchOS)
		if (platform != P_WATCH) {
			gamepad_svc = (VService){ "gamepad", 0x786300000000000aull, (CFDictionaryRef)[@{
				@"DeviceUsagePairs": @[ @{ @"DeviceUsagePage": @1, @"DeviceUsage": @5 } ],
				@"PrimaryUsagePage": @1, @"PrimaryUsage": @5,
				@"PhysicalDeviceUniqueID": @"SimulatedGamepad", @"GameControllerType": @1, @"Product": @"Gamepad" } retain] };
			add_service(system, &gamepad_svc);
		}
		if (platform == P_WATCH) {
			crown_svc = (VService){ "crown", 0x7863000000000006ull,
				make_props(@"com.apple.SimulatorHID.MainScreenDigitalCrownService", 0xff0b, 0x01, nil) };
			add_service(system, &crown_svc);
		}
	}
	// Models 2: the second integrated display of $SIMULATOR_CAPABILITIES (as host/simfb publishes it: SFB display
	// <screenID>, UID x86Sim-display-<screenID>) gets its own touchscreen, and the device a hinge sensor
	{
		const char* cp = getenv("SIMULATOR_CAPABILITIES");
		NSDictionary* caps = cp ? [NSDictionary dictionaryWithContentsOfFile:@(cp)] : nil;
		NSArray* disps = [caps[@"capabilities"] isKindOfClass:[NSDictionary class]] ? caps[@"capabilities"][@"displays"] : caps[@"displays"];
		int n = 0;
		for (NSDictionary* d in [disps isKindOfClass:[NSArray class]] ? disps : @[]) {
			if (![d isKindOfClass:[NSDictionary class]] || ![d[@"displayType"] isEqual:@"integrated"]) continue;
			if (n++ == 1) inner_display_id = [d[@"screenID"] unsignedIntValue];
		}
		if (n >= 2 && inner_display_id) {
			NSString* uid = [NSString stringWithFormat:@"x86Sim-display-%u", inner_display_id];
			inner_touch_svc = (VService){ "touchscreen-inner", 0x7863000000000004ull,
				make_props([@"com.apple.SimulatorHID.ScreenTouchService." stringByAppendingString:uid], 0x0d, 0x04, @{
					@"DisplayIntegrated": @YES, @"SupportsInk": @NO, @"displayUUID": uid }) };
			hinge_svc = (VService){ "hinge", 0x7863000000000005ull, make_props(@"x86Sim hinge", 0xff0c, 0x21, nil) };
			const char* h = getenv("SIMHID_HINGE");
			hinge_deg = h && *h ? atof(h) : 0;
			add_service(system, &inner_touch_svc);
			add_service(system, &hinge_svc);
			write_hinge_file(hinge_deg);
			avp_svc = (VService){ "avp", 0x7863000000000009ull, make_props(@"x86Sim AVP", 0xff61, 0x5b, nil) };
			add_service(system, &avp_svc);
			// the relay has no state until it gets all three inputs; locationd starts during the boot, after us:
			// the boot pose a few times over its first two minutes
			for (int i = 0; i < 6; i++)
				dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(5 + i * 20) * NSEC_PER_SEC),
					dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
						pthread_mutex_lock(&cmd_mu);
						avp_send_pose();
						avp_send_angle(hinge_deg);
						pthread_mutex_unlock(&cmd_mu);
					});
		}
	}
	// x86Sim arm64: an attached keyboard service puts UIKit in hardware-keyboard mode, so a text field never shows the
	// software keyboard. Attach it only when the host first types (like Xcode's simulator does); SIMHID_KEYBOARD=1: at start.
	hid_system = system;
	// Keyboard: a hardware keyboard disconnected before (x86sim keyboard hardware disconnect) stays so across boots, as
	// a device's setting in Apple's Simulator
	{
		int tok; uint64_t st = 0;
		if (notify_register_check("GSEventHardwareKeyboardAttached", &tok) == NOTIFY_STATUS_OK) {
			notify_get_state(tok, &st);
			notify_cancel(tok);
		}
		slog("GSEventHardwareKeyboardAttached at start: %llu", (unsigned long long)st);
	}
	hwkb_watch();
	if (!hwkb_saved_on()) {
		hwkb_set(false);
		// again once the system is up (notifyd and UIKit's readers start after backboardd)
		for (int i = 1; i <= 2; i++)
			dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)i * 20 * NSEC_PER_SEC), dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
				pthread_mutex_lock(&cmd_mu);
				if (!hwkb_on) hwkb_set(false);
				pthread_mutex_unlock(&cmd_mu);
			});
	}
	const char* kb = getenv("SIMHID_KEYBOARD");
	if (kb && *kb == '1')
		add_service(system, &keyboard_svc);
	else {  // Speed 34: idle detach of an on-demand keyboard (SIMHID_KEYBOARD=1 keeps it attached)
		const char* is = getenv("SIMHID_KB_IDLE_S");
		const int idle = is && *is ? atoi(is) : 30;
		// the thread also runs with SIMHID_KB_IDLE_S=0 (default "never"), so a client's kbidle still applies
		kb_default_idle_ns = idle > 0 ? (uint64_t)idle * 1000000000ull : ~0ull;
		pthread_t kt;
		pthread_create(&kt, NULL, kb_idle_thread, NULL);
		pthread_detach(kt);
	}
	pthread_t t;
	pthread_create(&t, NULL, listen_thread, NULL);
	pthread_detach(t);
	if (hinge_svc.service) {
		pthread_t ht;
		pthread_create(&ht, NULL, hinge_stream_thread, NULL);
		pthread_detach(ht);
	}
	slog("SimHIDSystem ready");
	return self;
}
@end
