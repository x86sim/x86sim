// x86Sim: simctl's device settings, done as CoreSimulatorBridge does them in a Mac's simulator (included by openurl.m;
// requests {"cmd": ...} through cli/libexec/devctl.py):
//   ui           simctl ui appearance / content_size / increase_contrast, plus Reduce Transparency and Reduce Motion:
//                UISUserInterfaceStyleMode -setModeValue: (UIKitServices; 1 light, 2 dark) and libAccessibility's
//                _AXSSet... (they write com.apple.Accessibility / com.apple.UIKit through cfprefsd and post the
//                notifications apps listen to)
//   statusbar    simctl status_bar override / clear / list: SystemStatus's status-bar-overrides domain
//                (STStatusBarOverridesStatusDomainPublisher; entitlements com.apple.systemstatus.publisher.domains and
//                com.apple.UIKit.status-bar-override-allow). The publisher lives as long as this job, as in
//                CoreSimulatorBridge; overrides accumulate until clear.
//   pasteboard   simctl pbcopy / pbpaste: UIPasteboard generalPasteboard (com.apple.Pasteboard.background-access,
//                paste-unchecked: no paste prompt), and its changeCount for a host clipboard sync
//   keychain     simctl keychain reset: SecItemDeleteAll (com.apple.private.security.delete.all)
//   globalprefs  AppleLanguages / AppleLocale in the global domain through cfprefsd (Settings > Language & Region)
// Every lookup is by name (dlopen / NSClassFromString): the same source builds for iOS, tvOS, watchOS and visionOS,
// and a class a platform lacks answers an error instead of failing the build.
#include <notify.h>

static NSDictionary* errorOf(NSString* fmt, ...) NS_FORMAT_FUNCTION(1, 2);
static NSDictionary* errorOf(NSString* fmt, ...) {
	va_list ap;
	va_start(ap, fmt);
	NSString* s = [[NSString alloc] initWithFormat:fmt arguments:ap];
	va_end(ap);
	return @{ @"ok": @NO, @"error": s };
}

static void* axLib(void) {
	static void* h;
	if (!h) h = dlopen("/usr/lib/libAccessibility.dylib", RTLD_NOW);
	return h;
}

// simctl content_size names <-> UIContentSizeCategory values
static NSDictionary* contentSizes(void) {
	return @{ @"extra-small": @"UICTContentSizeCategoryXS", @"small": @"UICTContentSizeCategoryS", @"medium": @"UICTContentSizeCategoryM",
		@"large": @"UICTContentSizeCategoryL", @"extra-large": @"UICTContentSizeCategoryXL", @"extra-extra-large": @"UICTContentSizeCategoryXXL",
		@"extra-extra-extra-large": @"UICTContentSizeCategoryXXXL", @"accessibility-medium": @"UICTContentSizeCategoryAccessibilityM",
		@"accessibility-large": @"UICTContentSizeCategoryAccessibilityL", @"accessibility-extra-large": @"UICTContentSizeCategoryAccessibilityXL",
		@"accessibility-extra-extra-large": @"UICTContentSizeCategoryAccessibilityXXL",
		@"accessibility-extra-extra-extra-large": @"UICTContentSizeCategoryAccessibilityXXXL" };
}

static NSArray* contentSizeOrder(void) {
	return @[ @"extra-small", @"small", @"medium", @"large", @"extra-large", @"extra-extra-large", @"extra-extra-extra-large",
		@"accessibility-medium", @"accessibility-large", @"accessibility-extra-large", @"accessibility-extra-extra-large",
		@"accessibility-extra-extra-extra-large" ];
}

static id styleMode(void) {
	static id m;
	if (!m) {
		dlopen("/System/Library/PrivateFrameworks/UIKitServices.framework/UIKitServices", RTLD_NOW);
		Class c = NSClassFromString(@"UISUserInterfaceStyleMode");
		if (c) m = MSG(id, id, SEL, id)(MSG(id, id, SEL)(c, sel_getUid("alloc")), sel_getUid("initWithDelegate:"), nil);
	}
	return m;
}

static NSString* currentContentSize(void) {
	CFStringRef (*copy)(void) = dlsym(axLib(), "_AXSCopyPreferredContentSizeCategoryName");
	NSString* v = copy ? CFBridgingRelease(copy()) : nil;
	if (!v) return @"large"; // nothing set: the default size
	for (NSString* k in contentSizes())
		if ([contentSizes()[k] isEqual:v]) return k;
	return v;
}

static BOOL axBool(const char* name) {
	unsigned char (*f)(void) = dlsym(axLib(), name);
	return f ? f() != 0 : NO;
}

static NSDictionary* uiState(void) {
	id m = styleMode();
	long mode = m ? MSG(long, id, SEL)(m, sel_getUid("modeValue")) : -1;
	return @{ @"ok": @YES, @"appearance": mode == 2 ? @"dark" : mode == 1 ? @"light" : [NSString stringWithFormat:@"%ld", mode],
		@"content_size": currentContentSize(), @"increase_contrast": axBool("_AXDarkenSystemColors") ? @"enabled" : @"disabled",
		@"reduce_transparency": axBool("_AXSEnhanceBackgroundContrastEnabled") ? @"enabled" : @"disabled",
		@"reduce_motion": axBool("_AXSReduceMotionEnabled") ? @"enabled" : @"disabled" };
}

static NSDictionary* uiSettings(NSDictionary* q) {
	NSString* what = q[@"what"];
	NSString* v = q[@"value"];
	if (![v isKindOfClass:NSString.class] || !v.length) return uiState();
	if ([what isEqual:@"appearance"]) {
		id m = styleMode();
		if (!m) return errorOf(@"no UISUserInterfaceStyleMode on this platform");
		long mode = [v isEqual:@"dark"] ? 2 : [v isEqual:@"light"] ? 1 : 0;
		if (!mode) return errorOf(@"appearance: light or dark");
		MSG(void, id, SEL, long)(m, sel_getUid("setModeValue:"), mode);
	} else if ([what isEqual:@"content_size"]) {
		NSString* name = v;
		NSArray* order = contentSizeOrder();
		if ([v isEqual:@"increment"] || [v isEqual:@"decrement"]) {
			NSUInteger i = [order indexOfObject:currentContentSize()];
			if (i == NSNotFound) i = 3;
			i = [v isEqual:@"increment"] ? MIN(i + 1, order.count - 1) : (i ? i - 1 : 0);
			name = order[i];
		}
		NSString* cat = contentSizes()[name];
		if (!cat) return errorOf(@"content_size: one of %@, increment, decrement", [order componentsJoinedByString:@", "]);
		void (*set)(CFStringRef) = dlsym(axLib(), "_AXSSetPreferredContentSizeCategoryName");
		void (*ext)(unsigned char) = dlsym(axLib(), "_AXSSetLargeTextUsesExtendedRange");
		if (!set) return errorOf(@"no _AXSSetPreferredContentSizeCategoryName");
		if (ext && [name hasPrefix:@"accessibility-"]) ext(1);
		set((__bridge CFStringRef)cat);
	} else {
		const char* fn = [what isEqual:@"increase_contrast"] ? "_AXSSetDarkenSystemColors"
			: [what isEqual:@"reduce_transparency"] ? "_AXSSetEnhanceBackgroundContrastEnabled"
			: [what isEqual:@"reduce_motion"] ? "_AXSSetReduceMotionEnabled" : NULL;
		if (!fn) return errorOf(@"ui: appearance, content_size, increase_contrast, reduce_transparency or reduce_motion");
		BOOL on = [v isEqual:@"enabled"] || [v isEqual:@"on"] || [v isEqual:@"1"];
		if (!on && ![v isEqual:@"disabled"] && ![v isEqual:@"off"] && ![v isEqual:@"0"]) return errorOf(@"%@: enabled or disabled", what);
		void (*set)(unsigned char) = dlsym(axLib(), fn);
		if (!set) return errorOf(@"no %s", fn);
		set(on);
	}
	// cfprefsd answers the read-back with the old value for a moment: report what was set
	NSMutableDictionary* st = [uiState() mutableCopy];
	if ([what isEqual:@"appearance"] || [what isEqual:@"content_size"]) return st;
	st[what] = ([v isEqual:@"enabled"] || [v isEqual:@"on"] || [v isEqual:@"1"]) ? @"enabled" : @"disabled";
	return st;
}

// ---- status bar ----

static id sbPublisher, sbOverrides; // STStatusBarOverridesStatusDomainPublisher, STMutableStatusBarData

static long cellStatus(NSString* mode) {
	// STStatusBarDataNetworkStatus: 0 unavailable, 1 disabled, 2 disconnected, 3 searching, 4 warning, 5 connected
	if ([mode isEqual:@"notSupported"]) return 0;
	if ([mode isEqual:@"searching"]) return 3;
	if ([mode isEqual:@"failed"]) return 2;
	if ([mode isEqual:@"active"]) return 5;
	return -1;
}

static NSString* sbTime(NSString* t) {
	// simctl takes a display string ("9:41") or an ISO 8601 date, shown as the status bar shows times
	NSISO8601DateFormatter* iso = [NSISO8601DateFormatter new];
	NSDate* d = [iso dateFromString:t];
	if (!d) return t;
	NSDateFormatter* f = [NSDateFormatter new];
	f.dateStyle = NSDateFormatterNoStyle;
	f.timeStyle = NSDateFormatterShortStyle;
	NSString* s = [f stringFromDate:d];
	// the status bar leaves out the AM/PM marker
	return [[[s stringByReplacingOccurrencesOfString:f.AMSymbol withString:@""] stringByReplacingOccurrencesOfString:f.PMSymbol withString:@""]
		stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceCharacterSet];
}

// the keys an STStatusBarData has (+[STStatusBarData entryKeys]; the instance has none of its own)
static id sbKeys(void) {
	return sbOverrides ? MSG(id, id, SEL)((id)[sbOverrides class], sel_getUid("entryKeys")) : @[];
}

static NSDictionary* sbList(void) {
	NSMutableDictionary* o = [NSMutableDictionary new];
	for (NSString* k in sbKeys()) {
		id e = MSG(id, id, SEL, id)(sbOverrides, sel_getUid("entryForKey:"), k);
		if (e) o[k] = [e description];
	}
	return @{ @"ok": @YES, @"overrides": o };
}

static void sbPublish(id data) {
	dispatch_semaphore_t done = dispatch_semaphore_create(0);
	MSG(void, id, SEL, id, id)(sbPublisher, sel_getUid("setData:completion:"), data, ^{ dispatch_semaphore_signal(done); });
	dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
}

static NSDictionary* statusBar(NSDictionary* q) {
	dlopen("/System/Library/PrivateFrameworks/SystemStatus.framework/SystemStatus", RTLD_NOW);
	Class pubC = NSClassFromString(@"STStatusBarOverridesStatusDomainPublisher"), dataC = NSClassFromString(@"STMutableStatusBarData"),
		ovC = NSClassFromString(@"STMutableStatusBarOverridesStatusDomainData");
	if (!pubC || !dataC || !ovC) return errorOf(@"status bar overrides are not supported on this platform");
	NSString* act = q[@"action"];
	if ([act isEqual:@"list"]) return sbList();
	if (!sbPublisher) sbPublisher = MSG(id, id, SEL)(MSG(id, id, SEL)(pubC, sel_getUid("alloc")), sel_getUid("init"));
	if ([act isEqual:@"clear"]) {
		sbOverrides = nil;
		sbPublish(MSG(id, id, SEL)(pubC, sel_getUid("emptyData")));
		return @{ @"ok": @YES, @"overrides": @{} };
	}
	if (![act isEqual:@"override"]) return errorOf(@"status_bar: override, clear or list");
	if (!sbOverrides) sbOverrides = MSG(id, id, SEL)(MSG(id, id, SEL)(dataC, sel_getUid("alloc")), sel_getUid("init"));
	id o = sbOverrides;
	Class strC = NSClassFromString(@"STStatusBarDataStringEntry"), batC = NSClassFromString(@"STStatusBarDataBatteryEntry"),
		cellC = NSClassFromString(@"STStatusBarDataCellularEntry"), wifiC = NSClassFromString(@"STStatusBarDataWifiEntry");
	if (q[@"time"]) {
		id e = MSG(id, id, SEL, id)(strC, sel_getUid("entryWithStringValue:"), sbTime(q[@"time"]));
		MSG(void, id, SEL, id)(o, sel_getUid("setTimeEntry:"), e);
		MSG(void, id, SEL, id)(o, sel_getUid("setShortTimeEntry:"), e);
	}
	if (q[@"batteryLevel"] || q[@"batteryState"]) {
		id old = MSG(id, id, SEL)(o, sel_getUid("mainBatteryEntry"));
		long cap = q[@"batteryLevel"] ? [q[@"batteryLevel"] longValue] : old ? MSG(long, id, SEL)(old, sel_getUid("capacity")) : 100;
		// STStatusBarDataBatteryState: 0 draining, 1 charging, 2 charged
		NSString* st = q[@"batteryState"];
		long s = st ? ([st isEqual:@"charging"] ? 1 : [st isEqual:@"charged"] ? 2 : [st isEqual:@"discharging"] ? 0 : -1)
			: old ? MSG(long, id, SEL)(old, sel_getUid("state")) : 0;
		if (s < 0) return errorOf(@"batteryState: charging, charged or discharging");
		if (cap < 0 || cap > 100) return errorOf(@"batteryLevel: 0..100");
		id e = MSG(id, id, SEL, long, long, BOOL, BOOL, id)(batC, sel_getUid("entryWithCapacity:state:saverMode:prominentlyShowsDetailString:detailString:"),
			cap, s, NO, NO, [NSString stringWithFormat:@"%ld%%", cap]);
		MSG(void, id, SEL, id)(o, sel_getUid("setMainBatteryEntry:"), e);
	}
	NSString* net = q[@"dataNetwork"];
	// STStatusBarDataCellularType: 0 none, 4 UMTS (3G), 5 4G, 6 LTE, 8 LTE-A, 9 LTE+, 10 5G, 11 5G+, 12 5G UWB, 13 5G UC
	NSDictionary* types = @{ @"hide": @0, @"wifi": @0, @"3g": @4, @"4g": @5, @"lte": @6, @"lte-a": @8, @"lte+": @9, @"5g": @10,
		@"5g+": @11, @"5g-uwb": @12, @"5g-uc": @13 };
	if (net && !types[net]) return errorOf(@"dataNetwork: hide, wifi, 3g, 4g, lte, lte-a, lte+, 5g, 5g+, 5g-uwb or 5g-uc");
	if (q[@"cellularMode"] || q[@"cellularBars"] || q[@"operatorName"] || net) {
		id old = MSG(id, id, SEL)(o, sel_getUid("cellularEntry"));
		long st = q[@"cellularMode"] ? cellStatus(q[@"cellularMode"]) : old ? MSG(long, id, SEL)(old, sel_getUid("status")) : 5;
		if (st < 0) return errorOf(@"cellularMode: notSupported, searching, failed or active");
		long bars = q[@"cellularBars"] ? [q[@"cellularBars"] longValue] : old ? MSG(long, id, SEL)(old, sel_getUid("displayValue")) : 4;
		if (bars < 0 || bars > 4) return errorOf(@"cellularBars: 0..4");
		long type = net ? [types[net] longValue] : old ? MSG(long, id, SEL)(old, sel_getUid("type")) : 10;
		NSString* name = q[@"operatorName"] ?: (old ? MSG(id, id, SEL)(old, sel_getUid("string")) : @"Carrier");
		id e = MSG(id, id, SEL, long, id, id, id, BOOL, BOOL, BOOL, BOOL, BOOL, long, BOOL, long, long, BOOL)(cellC,
			sel_getUid("entryWithType:stringValue:crossfadeStringValue:badgeStringValue:wifiCalling:callForwarding:showsSOSWhenDisabled:sosAvailable:isBootstrapCellular:status:lowDataMode:rawValue:displayValue:displayRawValue:"),
			type, name, nil, nil, NO, NO, NO, NO, NO, st, NO, bars, bars, NO);
		if (!e) return errorOf(@"no STStatusBarDataCellularEntry constructor");
		MSG(void, id, SEL, id)(o, sel_getUid("setCellularEntry:"), e);
	}
	if (q[@"wifiMode"] || q[@"wifiBars"] || net) {
		id old = MSG(id, id, SEL)(o, sel_getUid("wifiEntry"));
		long st = q[@"wifiMode"] ? cellStatus(q[@"wifiMode"]) : old ? MSG(long, id, SEL)(old, sel_getUid("status")) : 5;
		if (st < 0 || (st == 0 && q[@"wifiMode"])) return errorOf(@"wifiMode: searching, failed or active");
		if (st == 1) st = 5; // an earlier cellular data network's hidden Wi-Fi comes back
		long bars = q[@"wifiBars"] ? [q[@"wifiBars"] longValue] : old ? MSG(long, id, SEL)(old, sel_getUid("displayValue")) : 3;
		if (bars < 0 || bars > 3) return errorOf(@"wifiBars: 0..3");
		// a cellular data network (or hide) shows its type where the Wi-Fi icon was: no Wi-Fi entry
		// (+[STStatusBarDataEntry disabledEntry]; one with status "disabled" drew a crossed-out Wi-Fi icon)
		id e = net && ![net isEqual:@"wifi"] ? MSG(id, id, SEL)(wifiC, sel_getUid("disabledEntry"))
			: MSG(id, id, SEL, long, long, BOOL, long, long, BOOL)(wifiC, sel_getUid("entryWithType:status:lowDataMode:rawValue:displayValue:displayRawValue:"),
				0, st, NO, bars, bars, NO);
		MSG(void, id, SEL, id)(o, sel_getUid("setWifiEntry:"), e);
	}
	id d = MSG(id, id, SEL)(MSG(id, id, SEL)(ovC, sel_getUid("alloc")), sel_getUid("init"));
	for (NSString* k in sbKeys()) {
		id e = MSG(id, id, SEL, id)(o, sel_getUid("entryForKey:"), k);
		if (e) MSG(void, id, SEL, id, id)(d, sel_getUid("setEntry:forKey:"), e, k);
	}
	sbPublish(d);
	return sbList();
}

// ---- pasteboard ----

static id generalPasteboard(void) {
	dlopen("/System/Library/Frameworks/UIKit.framework/UIKit", RTLD_NOW);
	Class c = NSClassFromString(@"UIPasteboard");
	return c ? MSG(id, id, SEL)(c, sel_getUid("generalPasteboard")) : nil;
}

static NSDictionary* pasteboard(NSDictionary* q) {
	id pb = generalPasteboard();
	if (!pb) return errorOf(@"no UIPasteboard on this platform");
	NSString* act = q[@"action"];
	if ([act isEqual:@"set"]) {
		NSString* s = q[@"string"];
		if (![s isKindOfClass:NSString.class]) return errorOf(@"set: string required");
		MSG(void, id, SEL, id)(pb, sel_getUid("setString:"), s);
	} else if (![act isEqual:@"get"] && ![act isEqual:@"count"]) {
		return errorOf(@"pasteboard: get, set or count");
	}
	long n = MSG(long, id, SEL)(pb, sel_getUid("changeCount"));
	if ([act isEqual:@"count"]) return @{ @"ok": @YES, @"changeCount": @(n) };
	NSString* s = [act isEqual:@"get"] ? MSG(id, id, SEL)(pb, sel_getUid("string")) : q[@"string"];
	return @{ @"ok": @YES, @"changeCount": @(n), @"string": s ?: [NSNull null] };
}

// ---- keychain ----

static NSDictionary* keychainReset(void) {
	void* h = dlopen("/System/Library/Frameworks/Security.framework/Security", RTLD_NOW);
	unsigned char (*del)(void) = h ? dlsym(h, "SecItemDeleteAll") : NULL;
	if (!del) return errorOf(@"no SecItemDeleteAll");
	// it answers false when some items stay (system ones bound to the device), yet every app's items are gone
	return @{ @"ok": @YES, @"complete": @(del() != 0) };
}

// ---- language / region ----

static NSDictionary* globalPrefs(NSDictionary* q) {
	CFStringRef any = kCFPreferencesAnyApplication, user = kCFPreferencesCurrentUser, host = kCFPreferencesAnyHost;
	if ([q[@"languages"] isKindOfClass:NSArray.class]) CFPreferencesSetValue(CFSTR("AppleLanguages"), (__bridge CFArrayRef)q[@"languages"], any, user, host);
	if ([q[@"locale"] isKindOfClass:NSString.class]) CFPreferencesSetValue(CFSTR("AppleLocale"), (__bridge CFStringRef)q[@"locale"], any, user, host);
	CFPreferencesSynchronize(any, user, host);
	if (q[@"languages"] || q[@"locale"]) {
		// what Settings posts after a language / region change
		notify_post("com.apple.language.changed");
		notify_post("AppleLanguagePreferencesChangedNotification");
		notify_post("com.apple.system.timezone");
	}
	NSArray* langs = CFBridgingRelease(CFPreferencesCopyValue(CFSTR("AppleLanguages"), any, user, host));
	NSString* loc = CFBridgingRelease(CFPreferencesCopyValue(CFSTR("AppleLocale"), any, user, host));
	return @{ @"ok": @YES, @"languages": langs ?: @[], @"locale": loc ?: @"" };
}

// x86Sim: the status bar publisher's setup and its updates wait on the main queue (its first request never answered
// when run on the main thread, which this job's request loop is on): run it on a queue of its own while the main
// thread runs its run loop (which drains the main queue), for up to 20 s.
static NSDictionary* offMain(NSDictionary* (^work)(void)) {
	static dispatch_queue_t q;
	if (!q) q = dispatch_queue_create("com.x86sim.devctl", DISPATCH_QUEUE_SERIAL);
	__block NSDictionary* r = nil;
	__block volatile BOOL done = NO;
	dispatch_async(q, ^{
		NSDictionary* x = work();
		dispatch_async(dispatch_get_main_queue(), ^{ r = x; done = YES; });
	});
	NSDate* end = [NSDate dateWithTimeIntervalSinceNow:20];
	while (!done && [end timeIntervalSinceNow] > 0)
		[[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.05]];
	return r ?: errorOf(@"no answer from the status bar server in 20 s");
}

static NSDictionary* devctlRequest(NSString* c, NSDictionary* q) {
	if ([c isEqual:@"ui"]) return uiSettings(q);
	if ([c isEqual:@"statusbar"]) return offMain(^{ return statusBar(q); });
	if ([c isEqual:@"pasteboard"]) return pasteboard(q);
	if ([c isEqual:@"keychain"]) return keychainReset();
	if ([c isEqual:@"globalprefs"]) return globalPrefs(q);
	return nil;
}
