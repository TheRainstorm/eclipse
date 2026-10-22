/* Linux UAPI layouts stay in the system header; no guessed Rust union ABI. */
#include <linux/uhid.h>
#include <linux/input.h>
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
