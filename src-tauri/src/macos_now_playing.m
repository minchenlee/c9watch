#import <AppKit/AppKit.h>
#import <MediaPlayer/MediaPlayer.h>
#import <dispatch/dispatch.h>

static NSString *C9String(const char *value) {
    if (value == NULL) {
        return @"";
    }
    return [NSString stringWithUTF8String:value] ?: @"";
}

static void C9InstallRemoteCommands(void) {
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        MPRemoteCommandCenter *commands = [MPRemoteCommandCenter sharedCommandCenter];
        commands.playCommand.enabled = YES;
        commands.pauseCommand.enabled = YES;
        commands.togglePlayPauseCommand.enabled = YES;

        [commands.playCommand addTargetWithHandler:^MPRemoteCommandHandlerStatus(
            MPRemoteCommandEvent *_event
        ) {
            return MPRemoteCommandHandlerStatusSuccess;
        }];
        [commands.pauseCommand addTargetWithHandler:^MPRemoteCommandHandlerStatus(
            MPRemoteCommandEvent *_event
        ) {
            return MPRemoteCommandHandlerStatusSuccess;
        }];
        [commands.togglePlayPauseCommand addTargetWithHandler:^MPRemoteCommandHandlerStatus(
            MPRemoteCommandEvent *_event
        ) {
            return MPRemoteCommandHandlerStatusSuccess;
        }];
    });
}

void c9watch_update_now_playing(
    const char *title,
    const char *status,
    const char *project,
    const char *latest_message,
    bool is_playing
) {
    @autoreleasepool {
        NSString *titleString = [C9String(title) copy];
        NSString *statusString = [C9String(status) copy];
        NSString *projectString = [C9String(project) copy];
        NSString *latestMessageString = [C9String(latest_message) copy];
        BOOL playing = is_playing;

        // The polling loop runs on a worker thread. AppKit/MediaPlayer state
        // is published on the main queue so macOS can observe it reliably.
        dispatch_async(dispatch_get_main_queue(), ^{
            C9InstallRemoteCommands();
            NSMutableDictionary *info = [NSMutableDictionary dictionary];
            info[MPMediaItemPropertyTitle] = titleString;
            info[MPMediaItemPropertyArtist] = statusString;
            info[MPMediaItemPropertyAlbumTitle] = projectString;
            info[MPMediaItemPropertyGenre] = @"AI assistant";
            info[MPMediaItemPropertyMediaType] = @(MPMediaTypeMusic);
            info[MPMediaItemPropertyPersistentID] = @(1);
            info[MPMediaItemPropertyPlaybackDuration] = @(1.0);
            info[MPNowPlayingInfoPropertyElapsedPlaybackTime] = @(0.0);
            info[MPNowPlayingInfoPropertyPlaybackRate] = @(playing ? 1.0 : 0.0);

            // This field is not guaranteed to be rendered by macOS, but gives
            // system clients an additional lyric-like line when supported.
            if (latestMessageString.length > 0) {
                info[MPMediaItemPropertyComposer] = latestMessageString;
            }

            NSImage *artworkImage = [NSImage imageNamed:NSImageNameApplicationIcon];
            if (artworkImage != nil) {
                CGSize size = artworkImage.size;
                if (size.width <= 0 || size.height <= 0) {
                    size = CGSizeMake(512, 512);
                }
                MPMediaItemArtwork *artwork = [[MPMediaItemArtwork alloc]
                    initWithBoundsSize:size
                    requestHandler:^NSImage *(CGSize _requestedSize) {
                        (void)_requestedSize;
                        return artworkImage;
                    }];
                info[MPMediaItemPropertyArtwork] = artwork;
            }

            MPNowPlayingInfoCenter *center = [MPNowPlayingInfoCenter defaultCenter];
            center.nowPlayingInfo = info;
            center.playbackState = playing
                ? MPNowPlayingPlaybackStatePlaying
                : MPNowPlayingPlaybackStatePaused;
        });
    }
}

void c9watch_clear_now_playing(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        MPNowPlayingInfoCenter *center = [MPNowPlayingInfoCenter defaultCenter];
        center.nowPlayingInfo = nil;
        center.playbackState = MPNowPlayingPlaybackStateStopped;
    });
}
