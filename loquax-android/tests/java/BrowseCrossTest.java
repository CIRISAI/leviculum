package network.leviculum.loquax;

import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.List;
import java.util.Map;

/**
 * What a whole page costs Java to read (1650), on the host, run by
 * tests/browse.rs: the facade's answer for a long page, as the phone gets
 * it through JNI, decoded by the app's own {@link MsgPack}. Prints the
 * first decode (a cold JIT, as the first page after a start) and the
 * median of ten warm ones, and checks the lines all arrived.
 */
final class BrowseCrossTest {
    @SuppressWarnings("unchecked")
    public static void main(String[] args) throws Exception {
        byte[] bytes = Files.readAllBytes(Paths.get(args[0]));
        int want = Integer.parseInt(args[1]);
        long start = System.nanoTime();
        Map<String, Object> shown = (Map<String, Object>) MsgPack.unpack(bytes);
        long cold = System.nanoTime() - start;
        int lines = ((List<Object>) shown.get("lines")).size();
        if (lines != want) {
            throw new AssertionError("lines " + lines + " of " + want);
        }
        long[] warm = new long[10];
        for (int i = 0; i < warm.length; i++) {
            long at = System.nanoTime();
            MsgPack.unpack(bytes);
            warm[i] = System.nanoTime() - at;
        }
        java.util.Arrays.sort(warm);
        System.out.println("JAVA_DECODE bytes=" + bytes.length + " lines=" + lines
                + " cold_ms=" + cold / 1000000.0 + " warm_median_ms=" + warm[warm.length / 2] / 1000000.0);
        System.out.println("checks passed");
    }
}
