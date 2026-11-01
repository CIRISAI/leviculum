package network.leviculum.loquax;

import java.io.ByteArrayOutputStream;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;

/**
 * The recorder's level feed (1276) on the host, run by tests/fling.rs:
 * the PCM file `args[0]` at `args[1]` Hz fed to Levels in pieces of
 * `args[2]` bytes (odd, so a sample splits across two feeds), each 50 ms
 * window's RMS printed on a line of its own.
 */
final class LevelsTest {
    public static void main(String[] args) throws IOException {
        byte[] pcm = read(args[0]);
        int rate = Integer.parseInt(args[1]);
        int piece = Integer.parseInt(args[2]);
        final StringBuilder out = new StringBuilder();
        Levels levels = new Levels(rate, 50, new Levels.Sink() {
            @Override
            public void window(int rms) {
                out.append(rms).append('\n');
            }
        });
        for (int at = 0; at < pcm.length; at += piece) {
            levels.feed(pcm, at, Math.min(piece, pcm.length - at));
        }
        System.out.print(out);
        System.out.println("samples " + levels.samples());
    }

    private static byte[] read(String path) throws IOException {
        try (InputStream in = new FileInputStream(path)) {
            ByteArrayOutputStream all = new ByteArrayOutputStream();
            byte[] buffer = new byte[8192];
            int n;
            while ((n = in.read(buffer)) > 0) {
                all.write(buffer, 0, n);
            }
            return all.toByteArray();
        }
    }
}
