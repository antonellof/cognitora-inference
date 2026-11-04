// Report Apple Silicon energy use without root.
//
// Reads the "Energy Model" channels of IOReport, the system library behind
// powermetrics. Prints joules per channel when it gets SIGTERM or SIGINT.
//
//   clang -O2 mac_energy.c -o mac_energy -lIOReport \
//         -framework CoreFoundation
//
// IOReport is a private API: these declarations are not from a public header
// and may change between macOS releases, so check readings against a known
// load before trusting them.
#include <CoreFoundation/CoreFoundation.h>
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

typedef struct IOReportSubscription *IOReportSubscriptionRef;
extern CFDictionaryRef IOReportCopyChannelsInGroup(CFStringRef, CFStringRef, uint64_t, uint64_t, uint64_t);
extern IOReportSubscriptionRef IOReportCreateSubscription(void *, CFMutableDictionaryRef,
                                                          CFMutableDictionaryRef *, uint64_t, CFTypeRef);
extern CFDictionaryRef IOReportCreateSamples(IOReportSubscriptionRef, CFMutableDictionaryRef, CFTypeRef);
extern CFDictionaryRef IOReportCreateSamplesDelta(CFDictionaryRef, CFDictionaryRef, CFTypeRef);
extern CFStringRef IOReportChannelGetChannelName(CFDictionaryRef);
extern CFStringRef IOReportChannelGetUnitLabel(CFDictionaryRef);
extern int64_t IOReportSimpleGetIntegerValue(CFDictionaryRef, int32_t *);

static volatile sig_atomic_t stop_requested = 0;
static void on_signal(int sig) {
    (void)sig;
    stop_requested = 1;
}

static double to_joules(int64_t value, const char *unit) {
    if (strcmp(unit, "mJ") == 0) return value / 1e3;
    if (strcmp(unit, "uJ") == 0) return value / 1e6;
    if (strcmp(unit, "nJ") == 0) return value / 1e9;
    return 0.0;
}

int main(void) {
    signal(SIGTERM, on_signal);
    signal(SIGINT, on_signal);

    CFDictionaryRef group = IOReportCopyChannelsInGroup(CFSTR("Energy Model"), NULL, 0, 0, 0);
    if (!group) {
        fprintf(stderr, "no Energy Model channels\n");
        return 1;
    }
    CFMutableDictionaryRef channels =
        CFDictionaryCreateMutableCopy(kCFAllocatorDefault, 0, group);
    CFMutableDictionaryRef subscribed = NULL;
    IOReportSubscriptionRef sub = IOReportCreateSubscription(NULL, channels, &subscribed, 0, NULL);
    if (!sub) {
        fprintf(stderr, "IOReport subscription failed\n");
        return 1;
    }

    CFDictionaryRef first = IOReportCreateSamples(sub, subscribed, NULL);
    while (!stop_requested) pause();
    CFDictionaryRef second = IOReportCreateSamples(sub, subscribed, NULL);
    CFDictionaryRef delta = IOReportCreateSamplesDelta(first, second, NULL);

    CFArrayRef items = CFDictionaryGetValue(delta, CFSTR("IOReportChannels"));
    for (CFIndex i = 0; items && i < CFArrayGetCount(items); i++) {
        CFDictionaryRef ch = CFArrayGetValueAtIndex(items, i);
        char name[128] = "", unit[16] = "";
        CFStringGetCString(IOReportChannelGetChannelName(ch), name, sizeof name, kCFStringEncodingUTF8);
        CFStringRef u = IOReportChannelGetUnitLabel(ch);
        if (u) CFStringGetCString(u, unit, sizeof unit, kCFStringEncodingUTF8);
        double joules = to_joules(IOReportSimpleGetIntegerValue(ch, NULL), unit);
        if (joules > 0) printf("%s\t%.3f\n", name, joules);
    }
    return 0;
}
