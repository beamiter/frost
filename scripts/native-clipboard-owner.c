/* Genuine synthetic Wayland clipboard owner for hosted acceptance tests.
 * Own connection, nonblocking compositor-provided pipes, bounded work/lifetime.
 * Payloads are fixture files only. Logs contain metadata, never payload bodies. */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include "wlr-data-control-unstable-v1-client-protocol.h"
#define MAX_PAYLOAD (1024 * 1024 + 65536)
#define MAX_TRANSFERS 8
struct transfer { int fd; unsigned id; size_t bytes; double ready; };
static struct wl_display *display;
static struct wl_seat *seat;
static struct zwlr_data_control_manager_v1 *manager;
static struct zwlr_data_control_device_v1 *device;
static struct zwlr_data_control_source_v1 *source, *replacement;
static struct transfer transfers[MAX_TRANSFERS];
static unsigned next_id;
static unsigned char payload[MAX_PAYLOAD];
static size_t payload_size;
static const char *control, *scenario, *selection;
static bool finished, revoked;
static struct wl_callback *revoke_ack;
static double delay;
static double now(void) { struct timespec t; if (clock_gettime(CLOCK_MONOTONIC,&t)) exit(2); return t.tv_sec + t.tv_nsec/1e9; }
static bool marker(const char *name) { char p[4096]; if(snprintf(p,sizeof p,"%s/%s",control,name)>=(int)sizeof p)exit(2);return access(p,F_OK)==0; }
static void mark(const char *name) { char p[4096]; if(snprintf(p,sizeof p,"%s/%s",control,name)>=(int)sizeof p)exit(2);FILE *f=fopen(p,"w");if(!f)exit(2);fputs("ready\n",f);fclose(f); }
static void event(const char *phase,unsigned id,size_t bytes) { printf("{\"phase\":\"%s\",\"id\":%u,\"bytes\":%zu,\"monotonic\":%.9f}\n",phase,id,bytes,now());fflush(stdout); }
static void close_transfer(struct transfer *t,const char *reason) { event(reason,t->id,t->bytes);close(t->fd);t->fd=-1; }
static void send_data(void *data,struct zwlr_data_control_source_v1 *s,const char *mime,int32_t fd) {
 (void)data; (void)mime;
 if(s==replacement){close(fd);event("replacement-empty",0,0);return;}
 if(fcntl(fd,F_SETFL,fcntl(fd,F_GETFL)|O_NONBLOCK)<0){close(fd);exit(2);}
 for(unsigned i=0;i<MAX_TRANSFERS;i++)if(transfers[i].fd<0){
  transfers[i]=(struct transfer){.fd=fd,.id=++next_id,.ready=now()+delay};
  event("request",next_id,0);char name[48];snprintf(name,sizeof name,"request-%u",next_id);mark(name);return;
 }
 close(fd);event("provider-capacity-rejected",0,0);
}
static void cancelled(void *data,struct zwlr_data_control_source_v1 *s){(void)data;(void)s;event("source-cancelled",0,0);}
static const struct zwlr_data_control_source_v1_listener source_listener={.send=send_data,.cancelled=cancelled};
static void offered(void *d,struct zwlr_data_control_offer_v1 *o,const char *mime){(void)d;(void)o;(void)mime;}
static const struct zwlr_data_control_offer_v1_listener offer_listener={.offer=offered};
static void data_offer(void *d,struct zwlr_data_control_device_v1 *v,struct zwlr_data_control_offer_v1 *o){(void)d;(void)v;zwlr_data_control_offer_v1_add_listener(o,&offer_listener,NULL);}
static void selected(void *d,struct zwlr_data_control_device_v1 *v,struct zwlr_data_control_offer_v1 *o){(void)d;(void)v;if(o)zwlr_data_control_offer_v1_destroy(o);}
static void device_finished(void *d,struct zwlr_data_control_device_v1 *v){(void)d;(void)v;finished=true;}
static const struct zwlr_data_control_device_v1_listener device_listener={.data_offer=data_offer,.selection=selected,.finished=device_finished,.primary_selection=selected};
static void caps(void *d,struct wl_seat *s,uint32_t c){(void)d;(void)s;(void)c;}
static void seat_name(void *d,struct wl_seat *s,const char *n){(void)d;(void)s;(void)n;}
static const struct wl_seat_listener seat_listener={.capabilities=caps,.name=seat_name};
static void global(void *d,struct wl_registry *r,uint32_t n,const char *i,uint32_t v){
 (void)d;
 if(!strcmp(i,"wl_seat")&&!seat){seat=wl_registry_bind(r,n,&wl_seat_interface,1);wl_seat_add_listener(seat,&seat_listener,NULL);}
 if(!strcmp(i,"zwlr_data_control_manager_v1")&&v>=2)manager=wl_registry_bind(r,n,&zwlr_data_control_manager_v1_interface,2);
}
static void removed(void *d,struct wl_registry *r,uint32_t n){(void)d;(void)r;(void)n;}
static const struct wl_registry_listener registry_listener={.global=global,.global_remove=removed};
static struct zwlr_data_control_source_v1 *new_source(void){
 struct zwlr_data_control_source_v1 *s=zwlr_data_control_manager_v1_create_data_source(manager);
 zwlr_data_control_source_v1_add_listener(s,&source_listener,NULL);
 zwlr_data_control_source_v1_offer(s,"text/plain;charset=utf-8");
 zwlr_data_control_source_v1_offer(s,"text/plain");
 zwlr_data_control_source_v1_offer(s,"UTF8_STRING");return s;
}
static void set_source(struct zwlr_data_control_source_v1 *s){
 if(!strcmp(selection,"primary"))zwlr_data_control_device_v1_set_primary_selection(device,s);
 else zwlr_data_control_device_v1_set_selection(device,s);
}
static void revoke_done(void *data, struct wl_callback *callback, uint32_t serial){
 (void)data;(void)serial;wl_callback_destroy(callback);revoke_ack=NULL;
 event("revoked",0,0);mark("revoked");
}
static const struct wl_callback_listener revoke_listener={.done=revoke_done};
static void service(void){
 unsigned char continuous[4096];memset(continuous,'X',sizeof continuous);
 for(unsigned i=0;i<MAX_TRANSFERS;i++){
  struct transfer *t=&transfers[i];if(t->fd<0)continue;
  struct pollfd p={.fd=t->fd,.events=POLLOUT};if(poll(&p,1,0)<0)exit(2);
  if(p.revents&(POLLERR|POLLHUP|POLLNVAL)){close_transfer(t,"reader-closed");continue;}
  if(now()<t->ready||!strcmp(scenario,"stalled"))continue;
  size_t budget=65536;
  while(budget&&t->fd>=0){
   bool stream=!strcmp(scenario,"continuous");
   if(stream&&t->bytes>=16*1024*1024){close_transfer(t,"provider-safety-limit");break;}
   size_t left=stream?sizeof continuous:payload_size-t->bytes;
   if(!left){close_transfer(t,"complete");break;}
   size_t amount=left<budget?left:budget;
   ssize_t n=write(t->fd,stream?continuous:payload+t->bytes,amount);
   if(n>0){t->bytes+=(size_t)n;budget-=(size_t)n;}
   else if(n<0&&(errno==EAGAIN||errno==EWOULDBLOCK||errno==EINTR))break;
   else{close_transfer(t,"reader-closed");break;}
  }
 }
}
int main(int argc,char **argv){
 if(argc!=6){fprintf(stderr,"usage: owner CONTROL SCENARIO DELAY_MS clipboard|primary PAYLOAD\n");return 2;}
 control=argv[1];scenario=argv[2];selection=argv[4];char *end;long ms=strtol(argv[3],&end,10);
 if(*end||ms<0||ms>10000|| (strcmp(scenario,"finite")&&strcmp(scenario,"stalled")&&strcmp(scenario,"continuous")) || (strcmp(selection,"clipboard")&&strcmp(selection,"primary")))return 2;
 delay=ms/1000.0;FILE *f=fopen(argv[5],"rb");if(!f)return 2;payload_size=fread(payload,1,sizeof payload,f);if(ferror(f)||fgetc(f)!=EOF)return 2;fclose(f);
 signal(SIGPIPE,SIG_IGN);for(unsigned i=0;i<MAX_TRANSFERS;i++)transfers[i].fd=-1;
 display=wl_display_connect(NULL);if(!display)return 2;
 struct wl_registry *registry=wl_display_get_registry(display);wl_registry_add_listener(registry,&registry_listener,NULL);
 if(wl_display_roundtrip(display)<0||!seat||!manager)return 2;
 device=zwlr_data_control_manager_v1_get_data_device(manager,seat);zwlr_data_control_device_v1_add_listener(device,&device_listener,NULL);
 source=new_source();set_source(source);if(wl_display_roundtrip(display)<0)return 2;mark("ready");event("ready",0,payload_size);
 double deadline=now()+60;
 while(!finished&&!marker("stop")&&now()<deadline){
  if(marker("revoke")&&!revoked){replacement=new_source();set_source(replacement);revoked=true;event("revoke-requested",0,0);revoke_ack=wl_display_sync(display);wl_callback_add_listener(revoke_ack,&revoke_listener,NULL);}
  service();
  while(wl_display_prepare_read(display)!=0)if(wl_display_dispatch_pending(display)<0)return 2;
  if(wl_display_flush(display)<0&&errno!=EAGAIN){wl_display_cancel_read(display);return 2;}
  struct pollfd p={.fd=wl_display_get_fd(display),.events=POLLIN};int n=poll(&p,1,10);
  if(n>0&&(p.revents&POLLIN)){if(wl_display_read_events(display)<0)return 2;}
  else wl_display_cancel_read(display);
  if(n<0&&errno!=EINTR)return 2;
  if(wl_display_dispatch_pending(display)<0)return 2;
 }
 for(unsigned i=0;i<MAX_TRANSFERS;i++)if(transfers[i].fd>=0)close_transfer(&transfers[i],"provider-stop");
 event("stop",0,0);if(revoke_ack)wl_callback_destroy(revoke_ack);if(replacement)zwlr_data_control_source_v1_destroy(replacement);zwlr_data_control_source_v1_destroy(source);zwlr_data_control_device_v1_destroy(device);zwlr_data_control_manager_v1_destroy(manager);wl_seat_destroy(seat);wl_registry_destroy(registry);wl_display_disconnect(display);return finished?2:0;
}
