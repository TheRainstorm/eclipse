/* Linux UAPI layouts stay in the system header; no guessed Rust union ABI. */
#include <linux/uhid.h>
#include <linux/input.h>
#include <linux/uinput.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
static int emit(int fd, struct uhid_event *event) {
    ssize_t n; do {n=write(fd,event,sizeof(*event));} while(n<0 && errno==EINTR);
    return n==(ssize_t)sizeof(*event)?0:(n<0?-errno:-EIO);
}
int eclipse_pad_create(int fd,const char *unique,const unsigned char *descriptor,unsigned size) {
    if(size>HID_MAX_DESCRIPTOR_SIZE)return -EINVAL;
    struct uhid_event e={0};e.type=UHID_CREATE2;
    snprintf((char*)e.u.create2.name,sizeof(e.u.create2.name),"Eclipse virtual DS4");
    snprintf((char*)e.u.create2.phys,sizeof(e.u.create2.phys),"eclipse/virtual-gamepad");
    snprintf((char*)e.u.create2.uniq,sizeof(e.u.create2.uniq),"%s",unique);
    e.u.create2.bus=BUS_USB;e.u.create2.vendor=0x054c;e.u.create2.product=0x05c4;e.u.create2.version=0x0100;
    e.u.create2.rd_size=size;memcpy(e.u.create2.rd_data,descriptor,size);return emit(fd,&e);
}
int eclipse_pad_input(int fd,const unsigned char *bytes,unsigned size) {
    if(size>UHID_DATA_MAX)return -EINVAL;
    struct uhid_event e={0};e.type=UHID_INPUT2;e.u.input2.size=size;memcpy(e.u.input2.data,bytes,size);return emit(fd,&e);
}
int eclipse_pad_destroy(int fd){struct uhid_event e={0};e.type=UHID_DESTROY;return emit(fd,&e);}
int eclipse_pad_poll(int fd,unsigned *id,unsigned *number) {
    struct uhid_event e={0};ssize_t n;do{n=read(fd,&e,sizeof(e));}while(n<0 && errno==EINTR);
    if(n<0)return errno==EAGAIN?0:-errno;
    if(n<4)return -EIO;
    if(e.type==UHID_GET_REPORT){if(n<4+(ssize_t)sizeof(e.u.get_report))return -EPROTO;*id=e.u.get_report.id;*number=e.u.get_report.rnum;return 1;}
    if(e.type==UHID_SET_REPORT){if(n<4+(ssize_t)sizeof(e.u.set_report))return -EPROTO;*id=e.u.set_report.id;*number=e.u.set_report.rnum;return 2;}
    if(e.type==UHID_STOP)return -ENODEV;
    return 3;
}
int eclipse_pad_reply(int fd,unsigned id,unsigned number,const unsigned char *bytes,unsigned size) {
    if(size>UHID_DATA_MAX)return -EINVAL;
    struct uhid_event e={0};e.type=UHID_GET_REPORT_REPLY;e.u.get_report_reply.id=id;
    e.u.get_report_reply.err=size?0:EINVAL;e.u.get_report_reply.size=size;
    if(size)memcpy(e.u.get_report_reply.data,bytes,size);
    (void)number;return emit(fd,&e);
}
int eclipse_pad_set_reply(int fd,unsigned id){struct uhid_event e={0};e.type=UHID_SET_REPORT_REPLY;e.u.set_report_reply.id=id;e.u.set_report_reply.err=EINVAL;return emit(fd,&e);}

/* Xbox identity through the standard evdev/uinput interface. It has no motion
 * sensors; Nintendo/Sony controllers use the DS4 UHID device above instead. */
int eclipse_xpad_create(int fd) {
    if(ioctl(fd,UI_SET_EVBIT,EV_KEY)<0 || ioctl(fd,UI_SET_EVBIT,EV_ABS)<0)return -errno;
    const unsigned keys[]={BTN_SOUTH,BTN_EAST,BTN_WEST,BTN_NORTH,BTN_SELECT,BTN_MODE,BTN_START,BTN_THUMBL,BTN_THUMBR,BTN_TL,BTN_TR};
    for(unsigned i=0;i<sizeof(keys)/sizeof(keys[0]);i++)if(ioctl(fd,UI_SET_KEYBIT,keys[i])<0)return -errno;
    const unsigned axes[]={ABS_X,ABS_Y,ABS_RX,ABS_RY,ABS_Z,ABS_RZ,ABS_HAT0X,ABS_HAT0Y};
    for(unsigned i=0;i<8;i++){
        if(ioctl(fd,UI_SET_ABSBIT,axes[i])<0)return -errno;
        struct uinput_abs_setup axis={0};axis.code=axes[i];
        axis.absinfo.minimum=i<4?-32768:(i<6?0:-1);axis.absinfo.maximum=i<4?32767:(i<6?255:1);
        if(ioctl(fd,UI_ABS_SETUP,&axis)<0)return -errno;
    }
    struct uinput_setup setup={0};setup.id.bustype=BUS_USB;setup.id.vendor=0x045e;setup.id.product=0x028e;setup.id.version=0x0114;
    snprintf(setup.name,sizeof(setup.name),"Eclipse virtual Xbox 360");
    if(ioctl(fd,UI_DEV_SETUP,&setup)<0 || ioctl(fd,UI_DEV_CREATE)<0)return -errno;
    return 0;
}
int eclipse_xpad_input(int fd,unsigned buttons,const short *sticks,const unsigned char *triggers) {
    struct input_event events[20]={0};unsigned count=0;
    const unsigned keys[]={BTN_SOUTH,BTN_EAST,BTN_WEST,BTN_NORTH,BTN_SELECT,BTN_MODE,BTN_START,BTN_THUMBL,BTN_THUMBR,BTN_TL,BTN_TR};
    const unsigned masks[]={0x1000,0x2000,0x4000,0x8000,0x20,0x400,0x10,0x40,0x80,0x100,0x200};
    for(unsigned i=0;i<11;i++){events[count].type=EV_KEY;events[count].code=keys[i];events[count++].value=(buttons&masks[i])!=0;}
    const unsigned axes[]={ABS_X,ABS_Y,ABS_RX,ABS_RY,ABS_Z,ABS_RZ,ABS_HAT0X,ABS_HAT0Y};
    const int values[]={sticks[0],sticks[1],sticks[2],sticks[3],triggers[0],triggers[1],((buttons&8)!=0)-((buttons&4)!=0),((buttons&2)!=0)-((buttons&1)!=0)};
    for(unsigned i=0;i<8;i++){events[count].type=EV_ABS;events[count].code=axes[i];events[count++].value=values[i];}
    events[count].type=EV_SYN;events[count++].code=SYN_REPORT;
    ssize_t size=(ssize_t)(sizeof(events[0])*count),written;do{written=write(fd,events,size);}while(written<0&&errno==EINTR);
    return written==size?0:(written<0?-errno:-EIO);
}
int eclipse_xpad_destroy(int fd){return ioctl(fd,UI_DEV_DESTROY)<0?-errno:0;}
