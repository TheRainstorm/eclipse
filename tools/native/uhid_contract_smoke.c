/* Own UNIX sockets only: verify Linux UAPI messages without a virtual device. */
#include <linux/uhid.h>
#include <sys/socket.h>
#include <unistd.h>
#include <errno.h>
#include <assert.h>
#include <stdio.h>
#include <string.h>
int eclipse_pad_create(int,const char*,const unsigned char*,unsigned);
int eclipse_pad_input(int,const unsigned char*,unsigned);
int eclipse_pad_destroy(int);
int eclipse_pad_poll(int,unsigned*,unsigned*);
int eclipse_pad_reply(int,unsigned,unsigned,const unsigned char*,unsigned);
int eclipse_pad_set_reply(int,unsigned);
int main(void) {
    int fd[2];assert(socketpair(AF_UNIX,SOCK_SEQPACKET|SOCK_NONBLOCK,0,fd)==0);
    unsigned char descriptor[]={0x05,0x01,0xc0};struct uhid_event e;
    assert(eclipse_pad_create(fd[0],"02:00:00:00:00:01",descriptor,3)==0);
    assert(read(fd[1],&e,sizeof(e))==(ssize_t)sizeof(e));
    assert(e.type==UHID_CREATE2&&e.u.create2.bus==3&&e.u.create2.vendor==0x054c&&e.u.create2.product==0x05c4);
    assert(e.u.create2.rd_size==3&&!memcmp(e.u.create2.rd_data,descriptor,3));
    unsigned id=0,number=0;assert(eclipse_pad_poll(fd[0],&id,&number)==0);
    memset(&e,0,sizeof(e));e.type=UHID_GET_REPORT;e.u.get_report.id=123;e.u.get_report.rnum=2;
    assert(write(fd[1],&e,sizeof(e))==(ssize_t)sizeof(e));assert(eclipse_pad_poll(fd[0],&id,&number)==1&&id==123&&number==2);
    unsigned char data[]={2,3,4,5};assert(eclipse_pad_reply(fd[0],id,number,data,4)==0);
    assert(read(fd[1],&e,sizeof(e))==(ssize_t)sizeof(e));assert(e.type==UHID_GET_REPORT_REPLY&&e.u.get_report_reply.id==123&&e.u.get_report_reply.err==0&&e.u.get_report_reply.size==4&&!memcmp(e.u.get_report_reply.data,data,4));
    assert(eclipse_pad_reply(fd[0],123,99,data,0)==0);assert(read(fd[1],&e,sizeof(e))==(ssize_t)sizeof(e));assert(e.u.get_report_reply.err==EINVAL);
    unsigned char report[64]={1,2,3};assert(eclipse_pad_input(fd[0],report,64)==0);assert(read(fd[1],&e,sizeof(e))==(ssize_t)sizeof(e));assert(e.type==UHID_INPUT2&&e.u.input2.size==64&&!memcmp(e.u.input2.data,report,64));
    assert(eclipse_pad_input(fd[0],report,UHID_DATA_MAX+1)==-EINVAL);
    memset(&e,0,sizeof(e));e.type=UHID_GET_REPORT;assert(write(fd[1],&e,4)==4);assert(eclipse_pad_poll(fd[0],&id,&number)==-EPROTO);
    assert(eclipse_pad_destroy(fd[0])==0);assert(read(fd[1],&e,sizeof(e))==(ssize_t)sizeof(e));assert(e.type==UHID_DESTROY);
    close(fd[0]);close(fd[1]);puts("{\"uhid_abi\":true,\"owned_sockets\":true,\"kernel_device_created\":false}");return 0;
}
