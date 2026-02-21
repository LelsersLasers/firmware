#include "spmc.h"
#include "common/phal/rtc.h"

void rtc_config_cb(timestamped_frame_t*);
static void parse_gps_time(const timestamped_frame_t* msg, RTC_timestamp_t* gps_rtc_time);