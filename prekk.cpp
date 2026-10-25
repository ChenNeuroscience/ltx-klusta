// prekk.cpp : Defines the entry point for the console application.
//

#include <stdlib.h>
#include <iostream>
#include <iomanip>
#include <fstream>
#include <string>
#include <sstream>
#include <stdint.h>
#include <memory>
#include <stdexcept>
#include "size_checks.h"
#include "armadillo"  // see very helpful instructions here: http://www.ru.is/kennarar/sigurdurh/SCImplementationShort.pdf
#include <list>
#include "pca.h"

using namespace std;
using namespace arma;

typedef std::vector<double> stdvec;


vector<string> split(const std::string &s, char delim) {
	// Taken from: http://stackoverflow.com/a/236803/2399799
	vector<string> elems;
	stringstream ss(s);
	string item;
	while (std::getline(ss, item, delim)) {
		elems.push_back(item);
	}
	return elems;
}

static_assert(sizeof(void*) == 8, "Build prekk for a 64-bit target");
static_assert(sizeof(arma::uword) == 8, "Build with ARMA_64BIT_WORD");

int run(int argc, char* argv[])
{

	ifstream tetfile;
	if (argc < 5){
		cout << "All 4 arguments are required:" << endl << endl <<
			"1. path to tet file including extension" << endl <<
			"2. path to temporary file to write out the data for KlustaKwik, but without the .fet.n extension." << endl <<
			"3. path to temporary file to write out the value of num_spikes" << endl <<
			"        (this is for use in the batch file for choosing the subsample rate)\n" << endl <<
			"4. components to use, e.g: 3PC,A,VT" << endl <<
			"        (comma-delimited list, with no spaces. PC can be 0,1,2,3,...,9. VT is voltage at time of maximum variance. A is amplitude.)" << endl <<
			"Remember that if a file path contains spaces you need do \"C:\\quote\\the path\\in double-quotes.txt\"" << endl;
		return 1;
	}

	string tetfile_name = argv[1];
	string tmpfile_name = argv[2];
	string infofile_name = argv[3];
	vector<string> components = split(argv[4],',');
	int N_COMPS = 0;
	bool use_VT = false;
	bool use_A = false;


	for (size_t i = 0; i < components.size(); i++){
		string token = components[i];

		if (token == "VT"){
			use_VT = true;
			cout << "Using voltage at time of maximum variance." << endl;
		}else if (token == "A"){
			use_A = true;
			cout << "Using amplitude." << endl;
		}else if (token.substr(1) == "PC"){
			N_COMPS = std::stoi(token.substr(0, 1));
			cout << "Using " << N_COMPS << " principle components." << endl;
		}else{
			cout << "Unable to parse components argument: " << argv[4] << endl << "must be uppercase, and comma-delimited." << endl;
			return 1;
		}
	}

	tetfile.open(tetfile_name, ios::in | ios::binary);

	if (!tetfile.is_open()){
		cout << "could not open file with path:" << endl << tetfile_name;
		return 1;

	}

	string START_TOKEN = "data_start";
	string NUM_SPIKES_KEY = "num_spikes";

	string line;
	std::int64_t nSpikes = -1;
	streamoff p = 0;
	bool found_data = false;

	// data_start is followed immediately by binary data, not necessarily a newline.
	char header_byte;
	while (tetfile.get(header_byte)){
		line += header_byte;
		if (line == START_TOKEN){
			p = tetfile.tellg();
			found_data = true;
			break;
		}
		if (line.size() > 4096)
			throw std::runtime_error("Invalid tetrode header line");
		if (header_byte != '\n')
			continue;

		cout << endl << "    [key value] : " << line;

		if (line.compare(0, NUM_SPIKES_KEY.length(), NUM_SPIKES_KEY) == 0){
			stringstream line_;
			line_ << line.substr(NUM_SPIKES_KEY.length());
			if (!(line_ >> nSpikes))
				throw std::runtime_error("Invalid num_spikes in tetrode header");
		}
		line.clear();

	}

	if (!found_data)
		throw std::runtime_error("Missing data_start in tetrode header");
	const std::size_t bufferSize = prekk::checked_buffer_size(nSpikes);
	// Reject truncated data before allocating potentially many gigabytes.
	tetfile.seekg(0, ios::end);
	if (tetfile.tellg() - p < static_cast<streamoff>(bufferSize))
		throw std::runtime_error("Tetrode data is shorter than num_spikes requires");

	cout << endl << "found nSpikes=" << nSpikes <<endl	;
	//cout << endl << "data starts at byte " << (p) << endl;
	tetfile.close();


	//-------------------------------------------------------------------------------------------------
	// Write out number of spiks to a separte file to make it easy for the batch script to find this out.
	// Ideally we would read this from the original file in the batch script but the language is really limited.
	ofstream infofile;
	infofile.open(infofile_name);
	if (!infofile)
		throw std::runtime_error("Cannot open spike-count output file");
	infofile << nSpikes;
	infofile.close();
	//-------------------------------------------------------------------------------------------------


	int N_SAMPS = 50;
	int WAVE_LEN_BYTES = N_SAMPS + 4;
	int BYTES_PER_SPIKE = WAVE_LEN_BYTES * 4;
	std::unique_ptr<char[]> buffer(new char[bufferSize]);
	tetfile.open(tetfile_name, ios::in | ios::binary);

	tetfile.seekg(p);
	if (!tetfile.read(buffer.get(), static_cast<streamsize>(bufferSize)))
		throw std::runtime_error("Could not read all tetrode spike data");
	//cout << tetfile.tellg() << " " << tetfile.gcount() <<  endl;
	tetfile.close();
	Mat<char> data(buffer.get(), BYTES_PER_SPIKE, nSpikes, false);

	ofstream tmp;
	size_t ext_at = tetfile_name.find_last_of('.');
	if (ext_at == string::npos)
		throw std::runtime_error("Tetrode filename must have a numeric extension");
	tmpfile_name += ".fet" + tetfile_name.substr(ext_at);
	cout << tmpfile_name << endl;
	tmp.open(tmpfile_name);

	if (!tmp.is_open()){
		cout << "file not written";
		return 1;
	}





	// Decide which of the 4 channels is non-zero
	int cidx[] = { -1, -1, -1, -1 };
	int idx = 0;
	for (int c = 0; c < 4; c++){
		Mat<char> data_sub = data.rows(c * WAVE_LEN_BYTES + 4, c * WAVE_LEN_BYTES + WAVE_LEN_BYTES - 1); //select the 50 columns corresponding to the waves on the c'th channel.
		char sub_max = max(max(data_sub));
		char sub_min = min(min(data_sub));
		if (sub_max == sub_min){
			cout << "--no signal for chan " << c << ", so skipping it." << endl;
		}
		else{
			cidx[c] = idx;
				idx += 1;
		}
	}


	size_t n_cols = (N_COMPS + (use_A ? 1 : 0) + (use_VT ? 1 : 0))*idx;
	size_t col_i = 0;
	mat out = zeros<mat>(nSpikes, n_cols);

	// build PC columns in out matrix
	if (N_COMPS > 0){
		for (int c = 0; c < 4; c++){
			if (cidx[c] == -1)
				continue;

			mat data_sub = conv_to<mat>::from(data.rows(c * WAVE_LEN_BYTES + 4, c * WAVE_LEN_BYTES + WAVE_LEN_BYTES - 1).t()); //select the 50 columns corresponding to the waves on the c'th channel.

			const int num_variables = 50;
			const int num_records = 300;

			stats::pca pca(N_SAMPS);

			for (std::int64_t i = 0; i < nSpikes; i++) {
				vector<double> record(num_variables);
				pca.add_record(conv_to<stdvec>::from(data_sub.row(i)));
			}

			pca.set_num_retained(N_COMPS);
			pca.solve();

			for (int i = 0; i < N_COMPS; i++){
				out.col(col_i) = mat(pca.get_principal(i));
				col_i++;
			}
		}
		cout << "PCA done" << endl;
	}

	if (use_A){
		// Note we've actually already compute max/min values in order to check which channels are active, but lets do it again.
		for (int c = 0; c < 4; c++){
			if (cidx[c] == -1)
				continue;
			mat data_sub = conv_to<mat>::from(data.rows(c * WAVE_LEN_BYTES + 4, c * WAVE_LEN_BYTES + WAVE_LEN_BYTES - 1).t()); //select the 50 columns corresponding to the waves on the c'th channel.
			out.col(col_i) = arma::max(data_sub,1) - arma::min(data_sub,1);
			col_i++;
		}
		cout << "Amplitude done" << endl;
	}

	if (use_VT){
		for (int c = 0; c < 4; c++){
			if (cidx[c] == -1)
				continue;
			mat data_sub = conv_to<mat>::from(data.rows(c * WAVE_LEN_BYTES + 4, c * WAVE_LEN_BYTES + WAVE_LEN_BYTES - 1).t()); //select the 50 columns corresponding to the waves on the c'th channel.
			mat var = arma::var(data_sub, 0, 0);
			arma::uword max_ind;
			var.max(max_ind); //max_ind passed by non-const ref, and used as output
			out.col(col_i) = data_sub.col(max_ind); // select out only the voltage from the index with maximum variance
			col_i++;
		}
		cout << "Voltage-at-time-t done" << endl;
	}

	tmp << n_cols << endl;
	out.raw_print(tmp);
	tmp.close();
	if (!tmp)
		throw std::runtime_error("Could not write all feature data");
	return 0;
}

int main(int argc, char* argv[]) {
	try {
		return run(argc, argv);
	} catch (const std::bad_alloc&) {
		std::cerr << "prekk: Not enough memory for this recording. "
			"PCA requires several copies of the waveform data.\n";
	} catch (const std::exception& error) {
		std::cerr << "prekk: " << error.what() << '\n';
	}
	return 1;
}
