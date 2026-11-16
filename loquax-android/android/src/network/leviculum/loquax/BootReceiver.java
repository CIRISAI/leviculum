package network.leviculum.loquax;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.util.Log;

/**
 * After a reboot (1345): starts {@link LoquaxService} when the user had it
 * running, so the phone is reachable again without opening the app. A
 * service ended by its Stop stays ended (Notice.startsAtBoot).
 *
 * A BOOT_COMPLETED receiver is one of the exemptions from Android 12's
 * rule against starting a foreground service from the background
 * (developer.android.com/develop/background-work/services/fgs/restrictions-bg-start#background-start-restriction-exemptions).
 * From Android 15 on, which the APK targets, such a receiver may not
 * start the types dataSync, camera, mediaPlayback, phoneCall,
 * mediaProjection and microphone; remoteMessaging, the service's type, is
 * not among them
 * (developer.android.com/about/versions/15/behavior-changes-15#fgs-boot-completed).
 */
public class BootReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context context, Intent intent) {
        if (!Notice.startsAtBoot(intent.getAction(), LoquaxService.wanted(context))) {
            return;
        }
        try {
            LoquaxService.start(context);
        } catch (RuntimeException refused) {
            // Refused on this phone all the same: the app's next start
            // asks again, as before.
            Log.i(PhotoView.TAG, "start at boot refused: " + refused);
        }
    }
}
