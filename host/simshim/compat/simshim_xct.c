// x86Sim: XCTest attachments for `x86sim test` (issue #50). On a Mac, Xcode's IDE session receives every attachment a
// test adds (XCTAttachment(screenshot:), activities' attachments, an issue's failure screenshot) and keeps it in the
// result bundle. x86sim test runs XCTest without an IDE session, so they went nowhere. With
// X86SIM_XCTEST_ATTACHMENTS=<dir> (set by cli/libexec/xctest.py for the test process), each attachment's payload is
// written to <dir>/<n>.<ext> as it is added, and one JSON line to <dir>/index.jsonl: the test it was added in, its
// name, type, time (XCTAttachment.timestamp) and lifetime (0 keepAlways, 1 deleteOnSuccess), where it came from.
// xctest.py sorts them per test, drops the deleteOnSuccess ones of passed tests and lists the rest in results.json.
// An activity's attachment also names its activity ("activity": the XCActivityRecord's uuid), and every activity
// XCTest finishes is one line of <dir>/activities.jsonl: {"test", "uuid", "parent" (its parent activity's uuid, null
// at the top), "title", "type", "start", "finish" (Unix times)}: XCTest's activity tree (Xcode's
// ActionTestActivitySummary), which on a Mac goes to the IDE session. Each test it performs is one line of
// <dir>/tests.jsonl (when it ran).
#include <dlfcn.h>
#include <fcntl.h>
#include <mach-o/dyld.h>
#include <objc/message.h>
#include <objc/runtime.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <unistd.h>

static const char* xct_dir;
static pthread_mutex_t xct_mu = PTHREAD_MUTEX_INITIALIZER;
static char xct_test[512];   // -[Class test] of the test being invoked (main thread), "" between tests
static char xct_case[512];   // the same while it is performed (XCTest's "Start Test at" activity comes before invokeTest)
static unsigned xct_seq;
// attachments written already (one can pass two of the hooked methods): marked on the attachment itself, as a freed
// one's address is soon another's (a list of addresses dropped the failure triage's "App UI hierarchy" text)
static char xct_seen_key;
static IMP xct_invoke_orig, xct_act_orig, xct_issue_orig, xct_case_orig, xct_perform_orig, xct_finish_orig;

static const char* xct_cstr(id s)
{
	return s ? ((const char* (*)(id, SEL)) objc_msgSend)(s, sel_registerName("UTF8String")) : NULL;
}

static const char* xct_ext(const char* uti)
{
	static const struct { const char *uti, *ext; } m[] = {
		{ "public.png", "png" }, { "public.jpeg", "jpg" }, { "public.heic", "heic" }, { "public.tiff", "tiff" },
		{ "public.plain-text", "txt" }, { "public.utf8-plain-text", "txt" }, { "public.json", "json" },
		{ "com.apple.property-list", "plist" }, { "public.xml", "xml" }, { "public.html", "html" },
		{ "public.mpeg-4", "mp4" }, { "com.apple.quicktime-movie", "mov" }, { "public.zip-archive", "zip" },
		{ "com.pkware.zip-archive", "zip" }, { "public.data", "bin" },
	};
	for (size_t i = 0; uti && i < sizeof m / sizeof m[0]; i++)
		if (!strcmp(uti, m[i].uti))
			return m[i].ext;
	return "bin";
}

static void xct_json_str(FILE* f, const char* s)
{
	fputc('"', f);
	for (; s && *s; s++) {
		unsigned char c = (unsigned char) *s;
		if (c == '"' || c == '\\') fprintf(f, "\\%c", c);
		else if (c < 0x20) fprintf(f, "\\u%04x", c);
		else fputc(c, f);
	}
	fputc('"', f);
}

static const char* xct_uuid(id o, const char* sel)
{
	id u = o ? ((id (*)(id, SEL)) objc_msgSend)(o, sel_registerName(sel)) : NULL;
	return u ? xct_cstr(((id (*)(id, SEL)) objc_msgSend)(u, sel_registerName("UUIDString"))) : NULL;
}

static double xct_now(void);

static void xct_write(id att, const char* from, id activity)
{
	if (getenv("X86SIM_XCTEST_ATT_DEBUG")) { char b[160]; int k = snprintf(b, sizeof b, "simshim: attachment %p from %s\n", (void*) att, from); write(2, b, k); }
	if (!att || !xct_dir)
		return;
	pthread_mutex_lock(&xct_mu);
	if (objc_getAssociatedObject(att, &xct_seen_key)) { pthread_mutex_unlock(&xct_mu); return; }
	objc_setAssociatedObject(att, &xct_seen_key, (id) objc_getClass("NSObject"), OBJC_ASSOCIATION_ASSIGN);
	unsigned n = ++xct_seq;
	char test[512];
	snprintf(test, sizeof test, "%s", xct_test[0] ? xct_test : xct_case);
	pthread_mutex_unlock(&xct_mu);

	id payload = ((id (*)(id, SEL)) objc_msgSend)(att, sel_registerName("payload"));
	// XCTAttachment(string:), (screenshot:) ... serialize lazily: payload stays nil until it is accessed this way
	SEL acc = sel_registerName("accessPayloadWithBlock:");
	if (!payload && class_respondsToSelector(object_getClass(att), acc)) {
		__block id got = nil;
		void (^blk)(id) = ^(id data) { got = data ? ((id (*)(id, SEL)) objc_msgSend)(data, sel_registerName("copy")) : nil; };
		((void (*)(id, SEL, id)) objc_msgSend)(att, acc, (id) blk);
		payload = got;
	}
	const char* uti = xct_cstr(((id (*)(id, SEL)) objc_msgSend)(att, sel_registerName("uniformTypeIdentifier")));
	const char* name = xct_cstr(((id (*)(id, SEL)) objc_msgSend)(att, sel_registerName("name")));
	long lifetime = ((long (*)(id, SEL)) objc_msgSend)(att, sel_registerName("lifetime"));
	// XCTAttachment.timestamp (Xcode's attachment time in the result bundle); XCTest sets it when the attachment is
	// added (-captureTimestampIfMissing:), after this hook: now, then
	SEL tsel = sel_registerName("timestamp");
	id ts = class_respondsToSelector(object_getClass(att), tsel) ? ((id (*)(id, SEL)) objc_msgSend)(att, tsel) : nil;
	double when = ts ? ((double (*)(id, SEL)) objc_msgSend)(ts, sel_registerName("timeIntervalSince1970")) : xct_now();
	char file[64], path[1200];
	snprintf(file, sizeof file, "%u.%s", n, xct_ext(uti));
	snprintf(path, sizeof path, "%s/%s", xct_dir, file);
	bool ok = false;
	if (payload) {
		Class nss = objc_getClass("NSString");
		id p = ((id (*)(Class, SEL, const char*)) objc_msgSend)(nss, sel_registerName("stringWithUTF8String:"), path);
		ok = ((bool (*)(id, SEL, id, bool)) objc_msgSend)(payload, sel_registerName("writeToFile:atomically:"), p, true);
	}
	char idx[1200];
	snprintf(idx, sizeof idx, "%s/index.jsonl", xct_dir);
	pthread_mutex_lock(&xct_mu);
	FILE* f = fopen(idx, "a");
	if (getenv("X86SIM_XCTEST_ATT_DEBUG")) { char b[1400]; int k = snprintf(b, sizeof b, "simshim: attachment %u payload %p written %d index %p (%s)\n", n, (void*) payload, ok, (void*) f, idx); write(2, b, k); }
	if (f) {
		fputs("{\"test\":", f); xct_json_str(f, test);
		fputs(",\"name\":", f); xct_json_str(f, name);
		fputs(",\"uti\":", f); xct_json_str(f, uti);
		const char* au = xct_uuid(activity, "uuid");
		if (au) { fputs(",\"activity\":", f); xct_json_str(f, au); }
		if (when > 0) fprintf(f, ",\"time\":%.6f", when);
		fprintf(f, ",\"lifetime\":%ld,\"from\":\"%s\",\"seq\":%u,\"file\":", lifetime, from, n);
		if (ok) xct_json_str(f, file); else fputs("null", f);
		fputs("}\n", f);
		fclose(f);
	}
	pthread_mutex_unlock(&xct_mu);
}

static void xct_invoke(id self, SEL sel)
{
	const char* nm = xct_cstr(((id (*)(id, SEL)) objc_msgSend)(self, sel_registerName("name")));
	pthread_mutex_lock(&xct_mu);
	snprintf(xct_test, sizeof xct_test, "%s", nm ? nm : "");
	pthread_mutex_unlock(&xct_mu);
	((void (*)(id, SEL)) xct_invoke_orig)(self, sel);
	pthread_mutex_lock(&xct_mu);
	xct_test[0] = 0;
	pthread_mutex_unlock(&xct_mu);
}
static double xct_now(void)
{
	struct timeval tv;
	gettimeofday(&tv, NULL);
	return tv.tv_sec + tv.tv_usec / 1e6;
}

// every test XCTest performs is one line of <dir>/tests.jsonl: {"test", "start", "finish"} (Unix times), when it
// really ran (Xcode's ActionTestSummary times come from the IDE session's test-started / -finished messages)
static void xct_perform(id self, SEL sel, id run)
{
	const char* nm = xct_cstr(((id (*)(id, SEL)) objc_msgSend)(self, sel_registerName("name")));
	char name[512];
	snprintf(name, sizeof name, "%s", nm ? nm : "");
	pthread_mutex_lock(&xct_mu);
	snprintf(xct_case, sizeof xct_case, "%s", name);
	pthread_mutex_unlock(&xct_mu);
	double start = xct_now();
	((void (*)(id, SEL, id)) xct_perform_orig)(self, sel, run);
	double finish = xct_now();
	pthread_mutex_lock(&xct_mu);
	xct_case[0] = 0;
	if (xct_dir && name[0]) {
		char p[1200];
		snprintf(p, sizeof p, "%s/tests.jsonl", xct_dir);
		FILE* f = fopen(p, "a");
		if (f) {
			fputs("{\"test\":", f); xct_json_str(f, name);
			fprintf(f, ",\"start\":%.6f,\"finish\":%.6f}\n", start, finish);
			fclose(f);
		}
	}
	pthread_mutex_unlock(&xct_mu);
}

static double xct_time(id date)
{
	return date ? ((double (*)(id, SEL)) objc_msgSend)(date, sel_registerName("timeIntervalSince1970")) : 0;
}

// -[XCActivityRecord setFinish:]: the activity ended (XCTActivityRecordStack's _didFinishActivity:...)
static void xct_finish(id self, SEL sel, id date)
{
	((void (*)(id, SEL, id)) xct_finish_orig)(self, sel, date);
	if (!date || !xct_dir)
		return;
	const char* title = xct_cstr(((id (*)(id, SEL)) objc_msgSend)(self, sel_registerName("title")));
	const char* type = xct_cstr(((id (*)(id, SEL)) objc_msgSend)(self, sel_registerName("activityType")));
	const char* uuid = xct_uuid(self, "uuid");
	const char* parent = xct_uuid(self, "parentID");
	double start = xct_time(((id (*)(id, SEL)) objc_msgSend)(self, sel_registerName("start"))), finish = xct_time(date);
	char p[1200];
	snprintf(p, sizeof p, "%s/activities.jsonl", xct_dir);
	pthread_mutex_lock(&xct_mu);
	FILE* f = fopen(p, "a");
	if (f) {
		fputs("{\"test\":", f); xct_json_str(f, xct_test[0] ? xct_test : xct_case);
		fputs(",\"uuid\":", f); if (uuid) xct_json_str(f, uuid); else fputs("null", f);
		fputs(",\"parent\":", f); if (parent) xct_json_str(f, parent); else fputs("null", f);
		fputs(",\"title\":", f); xct_json_str(f, title);
		fputs(",\"type\":", f); xct_json_str(f, type);
		fprintf(f, ",\"start\":%.6f,\"finish\":%.6f}\n", start, finish);
		fclose(f);
	}
	pthread_mutex_unlock(&xct_mu);
}

static void xct_act_add(id self, SEL sel, id att) { xct_write(att, "activity", self); ((void (*)(id, SEL, id)) xct_act_orig)(self, sel, att); }
static void xct_issue_add(id self, SEL sel, id att) { xct_write(att, "issue", NULL); ((void (*)(id, SEL, id)) xct_issue_orig)(self, sel, att); }
static void xct_case_add(id self, SEL sel, id att) { xct_write(att, "test", NULL); ((void (*)(id, SEL, id)) xct_case_orig)(self, sel, att); }

static IMP xct_hook(const char* cls, const char* sel, IMP imp)
{
	Class c = objc_getClass(cls);
	Method m = c ? class_getInstanceMethod(c, sel_registerName(sel)) : NULL;
	return m ? method_setImplementation(m, imp) : NULL;
}

static void xct_added(const struct mach_header* mh, intptr_t slide)
{
	if (!xct_invoke_orig) xct_invoke_orig = xct_hook("XCTestCase", "invokeTest", (IMP) xct_invoke);
	if (!xct_act_orig) xct_act_orig = xct_hook("XCActivityRecord", "addAttachment:", (IMP) xct_act_add);
	if (!xct_issue_orig) xct_issue_orig = xct_hook("XCTMutableIssue", "addAttachment:", (IMP) xct_issue_add);
	if (!xct_case_orig) xct_case_orig = xct_hook("XCTestCase", "addAttachment:", (IMP) xct_case_add);
	if (!xct_perform_orig) xct_perform_orig = xct_hook("XCTestCase", "performTest:", (IMP) xct_perform);
	if (!xct_finish_orig) xct_finish_orig = xct_hook("XCActivityRecord", "setFinish:", (IMP) xct_finish);
	static bool said;
	if (!said && xct_invoke_orig) {
		said = true;
		char b[200];
		int k = snprintf(b, sizeof b, "simshim: XCTest attachments to %s (activity %d issue %d test %d)\n", xct_dir,
		                 xct_act_orig != NULL, xct_issue_orig != NULL, xct_case_orig != NULL);
		write(2, b, k);
	}
}

// x86Sim (issue #54): XCTest's button presses on an iPhone Duo. testmanagerd presses a button on the main display
// (XCUIDevice press(.home), and the home press that backgrounds the UI test runner before the first test) from Recap's
// touch screen digitizer sender (+[XCTouchGesturePerformer _buttonSenderForDisplayId:]), which carries the main
// display's UUID. SpringBoard routes buttons to itself by sender: with two or more always-connected displays (the
// Duo's cover and inner screens) its "SB-Default" rule takes only senders with no display (-[SBHIDEventDispatchController
// init]: nullDisplay alone, elsewhere builtinDisplay + nullDisplay), so backboardd found no destination ("destinations
// for Keyboard event: ()"), the press was lost and UI tests never started ("Failed to background test runner"). There,
// the press goes from Recap's phone button sender instead: the built-in buttons service, which has no display, as the
// device's own buttons (simhid's buttons service) do. Other devices keep Apple's choice. X86SIM_XCTEST_BUTTON_SENDER=0: off.
static IMP tm_btn_orig;
static int tm_multi = -1;   // the device has two or more always-connected displays (FBSDisplayMonitor, as SpringBoard)

static int tm_multi_display(void)
{
	if (tm_multi >= 0)
		return tm_multi;
	int n = 0;
	Class mc = objc_getClass("FBSDisplayMonitor");
	id m = mc ? ((id (*)(id, SEL)) objc_msgSend)(((id (*)(Class, SEL)) objc_msgSend)(mc, sel_registerName("alloc")), sel_registerName("init")) : nil;
	if (m) {
		id ids = ((id (*)(id, SEL)) objc_msgSend)(m, sel_registerName("alwaysConnectedIdentities"));
		n = ids ? (int) ((unsigned long (*)(id, SEL)) objc_msgSend)(ids, sel_registerName("count")) : 0;
		((void (*)(id, SEL)) objc_msgSend)(m, sel_registerName("invalidate"));
		((void (*)(id, SEL)) objc_msgSend)(m, sel_registerName("release"));
	}
	tm_multi = n >= 2;
	char b[120];
	int k = snprintf(b, sizeof b, "simshim: %d always-connected displays: XCTest buttons from the %s sender\n", n,
	                 tm_multi ? "phone button" : "default");
	write(2, b, k);
	return tm_multi;
}

static id tm_btn(id self, SEL sel, id display_id)
{
	id s = ((id (*)(id, SEL, id)) tm_btn_orig)(self, sel, display_id);
	Class rc = objc_getClass("RCPEventSenderProperties");
	if (!s || !rc || !tm_multi_display())
		return s;
	id touch = ((id (*)(Class, SEL)) objc_msgSend)(rc, sel_registerName("touchScreenDigitizerSender"));
	if (!touch || !((bool (*)(id, SEL, id)) objc_msgSend)(s, sel_registerName("isEqual:"), touch))
		return s;   // a button on another display: Apple's keyboard sender for that display
	id pb = ((id (*)(Class, SEL)) objc_msgSend)(rc, sel_registerName("phoneButtonSender"));
	return pb ? pb : s;
}

static void tm_added(const struct mach_header* mh, intptr_t slide)
{
	if (tm_btn_orig)
		return;
	Class c = objc_getClass("XCTouchGesturePerformer");
	Method m = c ? class_getClassMethod(c, sel_registerName("_buttonSenderForDisplayId:")) : NULL;
	if (m)
		tm_btn_orig = method_setImplementation(m, (IMP) tm_btn);
}

__attribute__((constructor)) static void tm_init(void)
{
	const char* e = getenv("X86SIM_XCTEST_BUTTON_SENDER");
	if (strcmp(getprogname(), "testmanagerd") || (e && *e == '0'))
		return;
	_dyld_register_func_for_add_image(tm_added);
}

__attribute__((constructor)) static void xct_init(void)
{
	const char* d = getenv("X86SIM_XCTEST_ATTACHMENTS");
	if (!d || !*d)
		return;
	xct_dir = strdup(d);
	_dyld_register_func_for_add_image(xct_added);
}
